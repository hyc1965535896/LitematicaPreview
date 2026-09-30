use std::cell::RefCell;
use std::fs::File;
use std::io::{Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use litematica_preview_native::{BlockReplacement, ExportFormat, LoadedPreview, PreviewOptions};
use nucleation::meshing::ResourcePackSource;

use crate::preview_process::DecoderProcess;
use crate::protocol;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoadOptions {
    #[serde(rename = "memoryLimitMB", deserialize_with = "required_nullable")]
    memory_limit_mb: Option<u16>,
    #[serde(deserialize_with = "required_nullable")]
    chunk_size: Option<u16>,
    #[serde(deserialize_with = "required_nullable")]
    thread_count: Option<u8>,
    speed_first: bool,
    #[serde(default)]
    replacements: Vec<BlockReplacement>,
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    serde::Deserialize::deserialize(deserializer)
}

impl LoadOptions {
    pub fn validate(self) -> Result<(PreviewOptions, Vec<BlockReplacement>), String> {
        let options = PreviewOptions {
            memory_limit_mb: self.memory_limit_mb,
            chunk_size: self.chunk_size,
            thread_count: self.thread_count,
            speed_first: self.speed_first,
        };
        options.validate()?;
        litematica_preview_native::validate_replacements(&self.replacements)?;
        Ok((options, self.replacements))
    }
}

struct QueuedBatch {
    batch: protocol::Batch,
    payload: Arc<protocol::Payload>,
}

#[derive(Default)]
struct StreamState {
    queue: std::collections::VecDeque<QueuedBatch>,
    leased: Option<QueuedBatch>,
    next_id: u64,
    complete: Option<protocol::Metadata>,
    error: Option<String>,
    waiting: bool,
    ended: bool,
}

struct StreamQueue {
    state: Mutex<StreamState>,
    changed: Condvar,
    cancelled: AtomicBool,
    capacity: usize,
}

impl StreamQueue {
    fn new(options: PreviewOptions) -> Self {
        Self {
            state: Mutex::new(StreamState::default()),
            changed: Condvar::new(),
            cancelled: AtomicBool::new(false),
            capacity: if options.speed_first {
                usize::from(options.thread_count.unwrap_or(2))
            } else {
                2
            },
        }
    }

    fn publish(&self, texture_offset: usize, payload: protocol::Payload) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "预览流不可用。")?;
        while state.queue.len() + usize::from(state.leased.is_some()) >= self.capacity
            && state.error.is_none()
        {
            // Keep the incoming payload outside the queue while the bounded window
            // is full. Waiting releases the lock so upload reads can proceed.
            state = self
                .changed
                .wait(state)
                .map_err(|_| "预览流不可用。")?;
        }
        if let Some(error) = &state.error {
            return Err(error.clone());
        }
        if state.complete.is_some() || state.ended {
            return Err("预览流已结束。".into());
        }
        state.next_id += 1;
        let batch = protocol::Batch {
            batch_id: state.next_id,
            texture_offset,
            metadata: payload.metadata.clone(),
        };
        state.queue.push_back(QueuedBatch {
            batch,
            payload: Arc::new(payload),
        });
        self.changed.notify_all();
        Ok(())
    }

    fn finish(&self, metadata: protocol::Metadata) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "预览流不可用。")?;
        if let Some(error) = &state.error {
            return Err(error.clone());
        }
        state.complete = Some(metadata);
        self.changed.notify_all();
        Ok(())
    }

    fn fail(&self, error: String) {
        self.cancelled.store(true, Ordering::Release);
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.queue.clear();
        state.leased = None;
        state.complete = None;
        state.error.get_or_insert(error);
        self.changed.notify_all();
    }

    fn next(&self, previous: Option<u64>) -> Result<protocol::StreamEvent, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "预览流不可用。")?;
        if let Some(error) = &state.error {
            return Err(error.clone());
        }
        if state.waiting
            || state.ended
            || state.leased.as_ref().map(|value| value.batch.batch_id) != previous
        {
            return Err("预览批次确认无效或重复。".into());
        }
        state.leased = None;
        state.waiting = true;
        self.changed.notify_all();
        loop {
            if let Some(error) = state.error.clone() {
                state.waiting = false;
                return Err(error);
            }
            if let Some(batch) = state.queue.pop_front() {
                let event = protocol::StreamEvent::Batch {
                    batch: batch.batch.clone(),
                };
                state.leased = Some(batch);
                state.waiting = false;
                return Ok(event);
            }
            if let Some(metadata) = state.complete.take() {
                state.waiting = false;
                state.ended = true;
                return Ok(protocol::StreamEvent::Complete { metadata });
            }
            state = self
                .changed
                .wait(state)
                .map_err(|_| "预览流不可用。")?;
        }
    }

    fn payload(&self, batch_id: u64) -> Result<Arc<protocol::Payload>, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "预览流不可用。")?;
        if let Some(error) = &state.error {
            return Err(error.clone());
        }
        state
            .leased
            .as_ref()
            .filter(|value| value.batch.batch_id == batch_id)
            .map(|value| Arc::clone(&value.payload))
            .ok_or_else(|| "预览批次未被租用或已释放。".into())
    }
}

#[derive(Default)]
pub struct PreviewWorker {
    generation: AtomicU64,
    active_request: AtomicU64,
    worker_pid: AtomicU32,
    process: Mutex<Option<DecoderProcess>>,
    payload: Mutex<Option<(u64, Arc<protocol::Payload>)>>,
    stream: Mutex<Option<(u64, Arc<StreamQueue>)>>,
}

impl PreviewWorker {
    pub fn begin_stream(&self, request_id: u64, options: PreviewOptions) -> Result<(), String> {
        options.validate()?;
        let mut stream = self
            .stream
            .lock()
            .map_err(|_| "预览流不可用。")?;
        self.ensure_current(request_id)?;
        if stream.as_ref().is_some_and(|(id, _)| *id == request_id) {
            return Err("预览流已经启动。".into());
        }
        if let Some((_, previous)) = stream.take() {
            previous.fail("Cancelled".into());
        }
        *stream = Some((request_id, Arc::new(StreamQueue::new(options))));
        self.active_request.store(request_id, Ordering::Release);
        Ok(())
    }

    fn stream(&self, request_id: u64) -> Result<Arc<StreamQueue>, String> {
        let stream = self
            .stream
            .lock()
            .map_err(|_| "预览流不可用。")?;
        self.ensure_current(request_id)?;
        stream
            .as_ref()
            .filter(|(id, _)| *id == request_id)
            .map(|(_, stream)| Arc::clone(stream))
            .ok_or_else(|| "预览流已被释放。".into())
    }

    pub fn fail_stream(&self, request_id: u64, error: String) {
        if let Ok(stream) = self.stream(request_id) {
            stream.fail(error);
        }
    }

    pub fn next(
        &self,
        request_id: u64,
        previous_batch_id: Option<u64>,
    ) -> Result<protocol::StreamEvent, String> {
        let stream = self.stream(request_id)?;
        let event = stream.next(previous_batch_id)?;
        self.ensure_current(request_id)?;
        if stream.cancelled.load(Ordering::Acquire) {
            return Err("Cancelled".into());
        }
        Ok(event)
    }

    pub fn load_stream(
        &self,
        path: &Path,
        pack_path: &Path,
        request_id: u64,
        options: PreviewOptions,
        replacements: &[BlockReplacement],
        on_progress: impl FnMut(u64, u64),
    ) -> Result<(), String> {
        options.validate()?;
        let stream = self.stream(request_id)?;
        let current =
            || self.ensure_current(request_id).is_ok() && !stream.cancelled.load(Ordering::Acquire);
        let result = (|| {
            let mut process = self
                .process
                .lock()
                .map_err(|_| "预览工作进程不可用，请重启应用。".to_string())?;
            if !current() {
                return Err("Cancelled".into());
            }
            if process
                .as_ref()
                .is_some_and(|process| process.memory_limit_mb() != options.memory_limit_mb)
            {
                self.worker_pid.store(0, Ordering::Release);
                process.take();
            }
            if process.is_none() {
                *process = Some(DecoderProcess::spawn(options.memory_limit_mb)?);
            }
            self.worker_pid
                .store(process.as_ref().unwrap().pid(), Ordering::Release);
            if !current() {
                return Err("Cancelled".into());
            }
            let result = process
                .as_mut()
                .ok_or("预览工作进程不可用。")?
                .load_stream(
                    path,
                    pack_path,
                    options.chunk_size,
                    options.thread_count,
                    options.speed_first,
                    replacements,
                    current,
                    on_progress,
                    |offset, payload| stream.publish(offset, payload),
                );
            match result {
                Ok(result) => result,
                Err(error) => {
                    // Notify the queue before dropping the broken decoder so consumers
                    // do not wait for process teardown.
                    stream.fail(error.clone());
                    self.worker_pid.store(0, Ordering::Release);
                    process.take();
                    Err(error)
                }
            }
        })();
        match result {
            Ok(metadata) => stream.finish(metadata),
            Err(error) => {
                stream.fail(error.clone());
                Err(error)
            }
        }
    }

    /// Writes a replacement-applied copy of `path` through the isolated
    /// decoder. Runs outside the preview session machinery: it neither
    /// disturbs the active request nor publishes payloads.
    pub fn export(
        &self,
        path: &Path,
        replacements: &[BlockReplacement],
        format: &str,
        destination: &Path,
        memory_limit_mb: Option<u16>,
    ) -> Result<protocol::Summary, String> {
        litematica_preview_native::validate_replacements(replacements)?;
        let mut process = self
            .process
            .lock()
            .map_err(|_| "预览工作进程不可用，请重启应用。".to_string())?;
        if process
            .as_ref()
            .is_some_and(|process| process.memory_limit_mb() != memory_limit_mb)
        {
            self.worker_pid.store(0, Ordering::Release);
            process.take();
        }
        if process.is_none() {
            *process = Some(DecoderProcess::spawn(memory_limit_mb)?);
        }
        self.worker_pid
            .store(process.as_ref().unwrap().pid(), Ordering::Release);
        let result = process
            .as_mut()
            .ok_or("预览工作进程不可用。")?
            .export(
                path,
                replacements,
                format,
                destination,
                || true,
            );
        match result {
            Ok(Ok(summary)) => Ok(summary),
            Ok(Err(error)) => Err(error),
            Err(error) => {
                self.worker_pid.store(0, Ordering::Release);
                process.take();
                Err(error)
            }
        }
    }
    pub fn begin_session(&self) -> u64 {
        let request_id = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.active_request.store(0, Ordering::Release);
        self.clear_older_payloads(request_id);
        request_id
    }

    pub fn advance(&self, request_id: u64) {
        self.generation.fetch_max(request_id, Ordering::AcqRel);
        if self.active_request.load(Ordering::Acquire) != self.generation.load(Ordering::Acquire) {
            self.active_request.store(0, Ordering::Release);
        }
        self.clear_older_payloads(self.generation.load(Ordering::Acquire));
    }

    pub fn ensure_current(&self, request_id: u64) -> Result<(), String> {
        if self.generation.load(Ordering::Acquire) == request_id {
            Ok(())
        } else {
            Err("Cancelled".into())
        }
    }

    pub fn preview_memory(&self, request_id: u64) -> Option<u64> {
        if self.generation.load(Ordering::Acquire) != request_id
            || self.active_request.load(Ordering::Acquire) != request_id
        {
            return None;
        }
        let pid = self.worker_pid.load(Ordering::Acquire);
        if pid == 0 {
            return None;
        }
        let memory = crate::preview_process::preview_memory(pid)?;
        (self.generation.load(Ordering::Acquire) == request_id
            && self.active_request.load(Ordering::Acquire) == request_id
            && self.worker_pid.load(Ordering::Acquire) == pid)
            .then_some(memory)
    }

    pub fn load(
        &self,
        path: &Path,
        pack_path: &Path,
        request_id: u64,
        options: PreviewOptions,
        replacements: &[BlockReplacement],
        on_progress: impl FnMut(u64, u64),
    ) -> Result<protocol::Metadata, String> {
        options.validate()?;
        self.ensure_current(request_id)?;
        let mut process = self
            .process
            .lock()
            .map_err(|_| "预览工作进程不可用，请重启应用。".to_string())?;
        self.ensure_current(request_id)?;
        if process
            .as_ref()
            .is_some_and(|process| process.memory_limit_mb() != options.memory_limit_mb)
        {
            self.active_request.store(0, Ordering::Release);
            self.worker_pid.store(0, Ordering::Release);
            process.take();
        }
        if process.is_none() {
            *process = Some(DecoderProcess::spawn(options.memory_limit_mb)?);
        }
        self.worker_pid
            .store(process.as_ref().unwrap().pid(), Ordering::Release);
        self.ensure_current(request_id)?;
        self.active_request.store(request_id, Ordering::Release);
        let result = process
            .as_mut()
            .ok_or("预览工作进程不可用。")?
            .load(
                path,
                pack_path,
                options.chunk_size,
                options.thread_count,
                options.speed_first,
                replacements,
                || self.ensure_current(request_id).is_ok(),
                on_progress,
            );
        let payload = match result {
            Ok(Err(error)) => {
                let _ = self.active_request.compare_exchange(
                    request_id,
                    0,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                return Err(error);
            }
            Ok(Ok(payload)) => payload,
            Err(error) => {
                let _ = self.active_request.compare_exchange(
                    request_id,
                    0,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                self.worker_pid.store(0, Ordering::Release);
                process.take();
                return Err(error);
            }
        };
        self.publish(request_id, payload)
    }

    pub fn read(
        &self,
        request_id: u64,
        batch_id: Option<u64>,
        ranges: &[protocol::ReadRange],
    ) -> Result<Vec<u8>, String> {
        let stream = batch_id.map(|_| self.stream(request_id)).transpose()?;
        let payload = if let Some(batch_id) = batch_id {
            stream.as_ref().unwrap().payload(batch_id)?
        } else {
            let stored = self
                .payload
                .lock()
                .map_err(|_| "预览缓冲区不可用。")?;
            self.ensure_current(request_id)?;
            let (_, payload) = stored
                .as_ref()
                .filter(|(id, _)| *id == request_id)
                .ok_or("预览缓冲区已被释放。")?;
            Arc::clone(payload)
        };
        // Release the publication lock before copying the requested packed ranges.
        let bytes = payload.read_ranges(ranges)?;
        self.ensure_current(request_id)?;
        if stream
            .as_ref()
            .is_some_and(|stream| stream.cancelled.load(Ordering::Acquire))
        {
            return Err("Cancelled".into());
        }
        Ok(bytes)
    }

    pub fn release(&self, request_id: u64) {
        if let Ok(mut stored) = self.stream.lock() {
            if stored.as_ref().is_some_and(|(id, _)| *id == request_id) {
                if let Some((_, stream)) = stored.take() {
                    stream.fail("Cancelled".into());
                }
            }
        }
        let _ = self.active_request.compare_exchange(
            request_id,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        // Do not let a stale upload release the newer request's buffers.
        if let Ok(mut stored) = self.payload.lock() {
            if stored.as_ref().is_some_and(|(id, _)| *id == request_id) {
                stored.take();
            }
        }
    }

    fn clear_older_payloads(&self, request_id: u64) {
        if let Ok(mut stored) = self.stream.lock() {
            if stored.as_ref().is_some_and(|(id, _)| *id < request_id) {
                if let Some((_, stream)) = stored.take() {
                    stream.fail("Cancelled".into());
                }
            }
        }
        if let Ok(mut stored) = self.payload.lock() {
            if stored.as_ref().is_some_and(|(id, _)| *id < request_id) {
                stored.take();
            }
        }
    }

    pub(crate) fn publish(
        &self,
        request_id: u64,
        payload: protocol::Payload,
    ) -> Result<protocol::Metadata, String> {
        let mut stored = self
            .payload
            .lock()
            .map_err(|_| "预览缓冲区不可用。")?;
        self.ensure_current(request_id)?;
        let metadata = payload.metadata.clone();
        *stored = Some((request_id, Arc::new(payload)));
        Ok(metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::{read_bytes, LoadOptions, PreviewOptions, PreviewWorker};

    fn range(buffer_id: usize, offset: usize, length: usize) -> crate::protocol::ReadRange {
        crate::protocol::ReadRange {
            buffer_id,
            offset,
            length,
        }
    }

    fn batch_id(event: crate::protocol::StreamEvent) -> u64 {
        match event {
            crate::protocol::StreamEvent::Batch { batch } => batch.batch_id,
            _ => panic!("Expected a batch"),
        }
    }

    #[test]
    fn streamed_protocol_overlaps_upload_with_bounded_backpressure() {
        use std::io::Write;
        use std::net::{TcpListener, TcpStream};
        use std::sync::{mpsc, Arc};
        use std::time::Duration;

        let worker = Arc::new(PreviewWorker::default());
        worker.advance(1);
        worker.begin_stream(1, PreviewOptions::default()).unwrap();
        assert!(worker.begin_stream(1, PreviewOptions::default()).is_err());
        let queue = worker.stream(1).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut sender = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut receiver, _) = listener.accept().unwrap();
        sender
            .write_all(&crate::protocol::tests::stream_fixture(4))
            .unwrap();
        let (arriving, arrivals) = mpsc::channel();
        let (published, publications) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        let producer = std::thread::spawn(move || {
            let mut id = 0;
            let result = crate::protocol::receive_stream(
                &mut receiver,
                || true,
                |_, _| {},
                |offset, payload| {
                    id += 1;
                    arriving.send(id).unwrap();
                    queue.publish(offset, payload)?;
                    published.send(id).unwrap();
                    Ok(())
                },
            )
            .unwrap()
            .unwrap();
            queue.finish(result).unwrap();
            done.send(()).unwrap();
        });
        let timeout = Duration::from_secs(5);
        assert_eq!(arrivals.recv_timeout(timeout).unwrap(), 1);
        assert_eq!(publications.recv_timeout(timeout).unwrap(), 1);
        assert_eq!(batch_id(worker.next(1, None).unwrap()), 1);
        assert_eq!(
            worker.read(1, Some(1), &[range(0, 0, 4)]).unwrap(),
            [255; 4]
        );
        assert_eq!(arrivals.recv_timeout(timeout).unwrap(), 2);
        assert_eq!(publications.recv_timeout(timeout).unwrap(), 2);
        assert_eq!(arrivals.recv_timeout(timeout).unwrap(), 3);
        assert!(matches!(
            publications.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(matches!(
            finished.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(worker.read(1, Some(2), &[range(0, 0, 4)]).is_err());
        assert!(worker.next(1, None).is_err());
        assert!(worker.next(1, Some(99)).is_err());
        let first = Arc::downgrade(&worker.stream(1).unwrap().payload(1).unwrap());
        assert_eq!(batch_id(worker.next(1, Some(1)).unwrap()), 2);
        assert!(first.upgrade().is_none());
        assert!(worker.read(1, Some(1), &[range(0, 0, 4)]).is_err());
        assert!(worker.next(1, Some(1)).is_err());
        assert_eq!(publications.recv_timeout(timeout).unwrap(), 3);
        assert_eq!(arrivals.recv_timeout(timeout).unwrap(), 4);
        assert_eq!(batch_id(worker.next(1, Some(2)).unwrap()), 3);
        assert_eq!(publications.recv_timeout(timeout).unwrap(), 4);
        assert_eq!(batch_id(worker.next(1, Some(3)).unwrap()), 4);
        let crate::protocol::StreamEvent::Complete { metadata } = worker.next(1, Some(4)).unwrap()
        else {
            panic!("Expected completion");
        };
        assert_eq!(metadata.triangle_count, 4);
        assert_eq!(metadata.textures.len(), 1);
        assert_eq!(metadata.byte_length, 4 + 4 * 39);
        assert!(worker.next(1, None).is_err());
        finished.recv_timeout(timeout).unwrap();
        producer.join().unwrap();
        assert_eq!(worker.active_request.load(super::Ordering::Acquire), 1);
        worker.release(1);
        assert_eq!(worker.active_request.load(super::Ordering::Acquire), 0);
    }

    #[test]
    fn late_failure_drops_queued_payloads_and_wakes_blocked_producer() {
        use std::sync::{mpsc, Arc};
        use std::time::Duration;
        let worker = PreviewWorker::default();
        worker.advance(1);
        worker.begin_stream(1, PreviewOptions::default()).unwrap();
        let queue = worker.stream(1).unwrap();
        queue
            .publish(0, crate::protocol::tests::chunk_payload())
            .unwrap();
        assert_eq!(batch_id(worker.next(1, None).unwrap()), 1);
        queue
            .publish(1, crate::protocol::tests::chunk_payload())
            .unwrap();
        let leased = Arc::downgrade(&queue.payload(1).unwrap());
        let queued = Arc::downgrade(
            &queue
                .state
                .lock()
                .expect("Queue must not be poisoned")
                .queue[0]
                .payload,
        );
        let (started, ready) = mpsc::channel();
        let producer_queue = Arc::clone(&queue);
        let producer = std::thread::spawn(move || {
            started.send(()).unwrap();
            producer_queue.publish(1, crate::protocol::tests::chunk_payload())
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.fail_stream(1, "Late decoder failure".into());
        assert_eq!(
            producer.join().unwrap().unwrap_err(),
            "Late decoder failure"
        );
        assert!(leased.upgrade().is_none());
        assert!(queued.upgrade().is_none());
        assert!(worker.read(1, Some(1), &[range(0, 0, 4)]).is_err());
        assert!(matches!(worker.next(1, Some(1)), Err(error) if error == "Late decoder failure"));
        assert_eq!(worker.active_request.load(super::Ordering::Acquire), 1);
        worker.release(1);
        assert_eq!(worker.active_request.load(super::Ordering::Acquire), 0);
    }

    #[test]
    fn cancellation_and_generation_changes_wake_waiters_without_poisoning_new_sessions() {
        use std::sync::{mpsc, Arc};
        use std::time::Duration;
        for advance in [false, true] {
            let worker = Arc::new(PreviewWorker::default());
            worker.advance(1);
            worker.begin_stream(1, PreviewOptions::default()).unwrap();
            let (started, ready) = mpsc::channel();
            let waiting = Arc::clone(&worker);
            let consumer = std::thread::spawn(move || {
                started.send(()).unwrap();
                waiting.next(1, None)
            });
            ready.recv_timeout(Duration::from_secs(5)).unwrap();
            if advance {
                worker.advance(2);
            } else {
                worker.release(1);
            }
            assert!(consumer.join().unwrap().is_err());
            worker.advance(2);
            worker.begin_stream(2, PreviewOptions::default()).unwrap();
            worker.fail_stream(1, "Stale failure".into());
            worker.release(1);
            worker
                .stream(2)
                .unwrap()
                .publish(0, crate::protocol::tests::chunk_payload())
                .unwrap();
            assert_eq!(batch_id(worker.next(2, None).unwrap()), 1);
            worker.release(2);
        }
    }

    #[test]
    fn release_unblocks_a_producer_and_late_transport_failure_revokes_a_lease() {
        use std::io::Write;
        use std::net::{Shutdown, TcpListener, TcpStream};
        use std::sync::{mpsc, Arc};
        use std::time::Duration;

        let worker = PreviewWorker::default();
        worker.advance(1);
        worker.begin_stream(1, PreviewOptions::default()).unwrap();
        let queue = worker.stream(1).unwrap();
        queue
            .publish(0, crate::protocol::tests::chunk_payload())
            .unwrap();
        queue
            .publish(1, crate::protocol::tests::chunk_payload())
            .unwrap();
        let pending = Arc::clone(&queue);
        let (entering, entered) = mpsc::channel();
        let producer = std::thread::spawn(move || {
            entering.send(()).unwrap();
            pending.publish(1, crate::protocol::tests::chunk_payload())
        });
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.release(1);
        assert!(producer.join().unwrap().is_err());
        assert!(queue.cancelled.load(super::Ordering::Acquire));

        worker.advance(2);
        worker.begin_stream(2, PreviewOptions::default()).unwrap();
        let queue = worker.stream(2).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut sender = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut receiver, _) = listener.accept().unwrap();
        let mut truncated = crate::protocol::tests::stream_fixture(1);
        truncated.pop();
        sender.write_all(&truncated).unwrap();
        sender.shutdown(Shutdown::Write).unwrap();
        let (resume, wait) = mpsc::channel();
        let producer = std::thread::spawn(move || {
            let result = crate::protocol::receive_stream(
                &mut receiver,
                || true,
                |_, _| {},
                |offset, payload| {
                    queue.publish(offset, payload)?;
                    wait.recv_timeout(Duration::from_secs(5)).unwrap();
                    Ok(())
                },
            );
            let error = match result {
                Err(error) => error.to_string(),
                _ => panic!("Truncated terminator was accepted"),
            };
            queue.fail(error);
        });
        assert_eq!(batch_id(worker.next(2, None).unwrap()), 1);
        assert_eq!(
            worker.read(2, Some(1), &[range(0, 0, 4)]).unwrap(),
            [255; 4]
        );
        let leased = Arc::downgrade(&worker.stream(2).unwrap().payload(1).unwrap());
        resume.send(()).unwrap();
        producer.join().unwrap();
        assert!(leased.upgrade().is_none());
        assert!(worker.next(2, Some(1)).is_err());
        assert!(worker.read(2, Some(1), &[range(0, 0, 4)]).is_err());
        worker.release(2);
    }

    #[test]
    fn reload_and_delayed_requests_cannot_revive_stale_work() {
        let worker = PreviewWorker::default();
        worker.advance(20);
        worker.advance(19);
        assert!(worker.ensure_current(20).is_ok());
        assert!(worker.ensure_current(19).is_err());

        let reloaded = worker.begin_session();
        assert!(worker.ensure_current(20).is_err());
        worker.advance(reloaded + 1);
        worker.advance(20);
        assert!(worker.ensure_current(reloaded + 1).is_ok());
        assert!(worker.ensure_current(reloaded).is_err());
    }

    fn options(value: serde_json::Value) -> Result<PreviewOptions, String> {
        serde_json::from_value::<LoadOptions>(value)
            .map_err(|error| error.to_string())?
            .validate()
            .map(|(options, _)| options)
    }

    #[test]
    fn preview_options_require_explicit_nullable_integer_settings() {
        use serde_json::json;

        let valid = json!({"memoryLimitMB": 2048, "chunkSize": 64, "threadCount": null, "speedFirst": false});
        assert_eq!(
            options(valid.clone()).unwrap(),
            PreviewOptions {
                memory_limit_mb: Some(2048),
                ..PreviewOptions::default()
            }
        );
        assert_eq!(
            options(json!({"memoryLimitMB": null, "chunkSize": null, "threadCount": null, "speedFirst": false}))
                .unwrap(),
            PreviewOptions {
                memory_limit_mb: None,
                chunk_size: None,
                thread_count: None,
                speed_first: false,
            }
        );
        for field in ["memoryLimitMB", "chunkSize", "threadCount", "speedFirst"] {
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(options(missing).is_err());
        }
        for (field, value) in [
            ("memoryLimitMB", json!(2047)),
            ("memoryLimitMB", json!(8193)),
            ("memoryLimitMB", json!(2048.5)),
            ("chunkSize", json!(48)),
            ("chunkSize", json!(64.5)),
            ("threadCount", json!(2.5)),
            ("threadCount", json!("2")),
            ("speedFirst", json!(1)),
            ("speedFirst", json!(null)),
            ("extra", json!(true)),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            assert!(options(invalid).is_err(), "accepted invalid {field}");
        }
    }

    #[test]
    fn threading_requires_chunking_and_available_worker_capacity() {
        use serde_json::json;
        let maximum = litematica_preview_native::max_worker_threads();
        let request = |chunk_size, count| {
            json!({
                "memoryLimitMB": null, "chunkSize": chunk_size, "threadCount": count, "speedFirst": false,
            })
        };
        assert!(options(request(Some(64), 1)).is_err());
        assert!(options(request(Some(64), maximum + 1)).is_err());
        assert!(options(request(None, 2)).is_err());
        if maximum >= 2 {
            let parsed = options(request(Some(64), maximum)).unwrap();
            assert_eq!(parsed.thread_count, Some(maximum));
        } else {
            assert!(options(request(Some(64), 2)).is_err());
        }
    }

    #[test]
    fn speed_first_requires_multithreading_but_preserves_decoder_memory_cap() {
        use serde_json::json;
        let mut request = json!({"memoryLimitMB": null, "chunkSize": 64, "threadCount": null, "speedFirst": true});
        assert!(options(request.clone()).is_err());
        if litematica_preview_native::max_worker_threads() < 2 {
            return;
        }
        request["threadCount"] = json!(2);
        assert!(options(request.clone()).unwrap().speed_first);
        request["memoryLimitMB"] = json!(2048);
        let capped = options(request.clone()).unwrap();
        assert!(capped.speed_first);
        assert_eq!(capped.memory_limit_mb, Some(2048));
        request["speedFirst"] = json!(false);
        assert_eq!(options(request).unwrap().memory_limit_mb, Some(2048));
    }

    #[test]
    fn speed_first_queue_uses_selected_worker_window_with_backpressure() {
        use std::sync::{mpsc, Arc};
        use std::time::Duration;
        let count = litematica_preview_native::max_worker_threads();
        if count < 3 {
            return;
        }
        let worker = PreviewWorker::default();
        worker.advance(1);
        worker
            .begin_stream(
                1,
                PreviewOptions {
                    thread_count: Some(count),
                    speed_first: true,
                    ..PreviewOptions::default()
                },
            )
            .unwrap();
        let queue = worker.stream(1).unwrap();
        for _ in 0..count {
            queue
                .publish(0, crate::protocol::tests::chunk_payload())
                .unwrap();
        }
        assert_eq!(batch_id(worker.next(1, None).unwrap()), 1);
        let (started, ready) = mpsc::channel();
        let (sent, received) = mpsc::channel();
        let publishing = Arc::clone(&queue);
        let producer = std::thread::spawn(move || {
            started.send(()).unwrap();
            let result = publishing.publish(0, crate::protocol::tests::chunk_payload());
            sent.send(result).unwrap();
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert_eq!(batch_id(worker.next(1, Some(1)).unwrap()), 2);
        received
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        producer.join().unwrap();
        worker.release(1);
    }

    #[test]
    fn file_reads_follow_actual_bytes_instead_of_declared_file_quota() {
        let bytes = [1, 2, 3];
        assert_eq!(
            read_bytes(&bytes[..], 1024 * 1024 * 1024 + 1, || Ok(())).unwrap(),
            bytes
        );
        let error = read_bytes(&bytes[..], 1, || Err("Cancelled".into())).unwrap_err();
        assert_eq!(error, "Cancelled");
        assert!(read_bytes(&[][..], 3, || Ok(())).is_err());
        assert!(read_bytes(&bytes[..], u64::MAX, || Ok(())).is_err());
    }
}

/// Decodes a schematic inside the isolated decoder process.
///
/// The coordinator sends ordered chunks while native workers retain a bounded result
/// set for the current request. Only the resource pack survives across requests.
pub(crate) fn decode(
    path: &Path,
    pack_path: &Path,
    pack: &mut Option<ResourcePackSource>,
    options: PreviewOptions,
    replacements: &[BlockReplacement],
    stream: &mut (impl Read + Write),
) -> Result<LoadedPreview, String> {
    options.validate()?;
    let result = catch_unwind(AssertUnwindSafe(|| {
        let encoder = RefCell::new(protocol::Encoder::new(stream));
        let mut last_progress = None;
        let current = || encoder.borrow_mut().checkpoint();
        let data = read_file(path, &current)?;
        if pack.is_none() {
            let bytes = read_file(pack_path, &current)?;
            *pack = Some(
                ResourcePackSource::from_bytes(&bytes)
                    .map_err(|e| format!("随附的方块资源无效：{e}"))?,
            );
        }
        litematica_preview_native::load_chunks(
            &data,
            pack.as_ref()
                .ok_or("随附的方块资源不可用。")?,
            options,
            replacements,
            |preview| encoder.borrow_mut().chunk(preview),
            |completed, total| {
                let now = Instant::now();
                if completed == 0
                    || completed == total
                    || last_progress
                        .is_none_or(|last| now.duration_since(last) >= Duration::from_millis(150))
                {
                    encoder
                        .borrow_mut()
                        .progress(completed as u64, total as u64)?;
                    last_progress = Some(now);
                }
                Ok(())
            },
            current,
        )
    }));
    finish_catch_unwind(pack, result)
}

/// Worker-side export: decode, apply replacements, serialize, and write the
/// destination file. The resource pack is not needed for re-serialization.
pub(crate) fn export_to_file(
    path: &Path,
    replacements: &[BlockReplacement],
    format: &str,
    destination: &Path,
) -> Result<LoadedPreview, String> {
    litematica_preview_native::validate_replacements(replacements)?;
    let format = ExportFormat::from_extension(format)
        .ok_or_else(|| format!("不受支持的导出格式：{format}"))?;
    let data = read_file(path, &|| Ok(()))?;
    let exported = litematica_preview_native::export_schematic(&data, replacements, format)?;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("无法创建导出目录：{e}"))?;
    }
    let mut temp = destination.to_path_buf();
    temp.set_extension("part");
    std::fs::write(&temp, &exported.data).map_err(|e| format!("无法写入导出文件：{e}"))?;
    std::fs::rename(&temp, destination).map_err(|e| format!("无法完成导出文件：{e}"))?;
    Ok(LoadedPreview {
        info: litematica_preview_native::PreviewInfo {
            block_count: exported.block_count,
            ..litematica_preview_native::PreviewInfo::default()
        },
        materials: Vec::new(),
        replaced: exported.replaced,
    })
}

fn finish_catch_unwind(
    pack: &mut Option<ResourcePackSource>,
    result: std::thread::Result<Result<LoadedPreview, String>>,
) -> Result<LoadedPreview, String> {
    result.unwrap_or_else(|panic| {
        // Discard native state mutated during the panic.
        *pack = None;
        let detail = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or("Unknown native panic");
        Err(format!(
            "解码器遇到内部错误：{detail}"
        ))
    })
}

fn read_file(path: &Path, current: impl Fn() -> Result<(), String>) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|e| format!("无法打开 {}：{e}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("无法检查 {}：{e}", path.display()))?;
    if !metadata.is_file() {
        return Err("请选择投影文件，而不是文件夹或设备。".into());
    }
    read_bytes(file, metadata.len(), current)
}

fn read_bytes(
    mut reader: impl Read,
    declared_length: u64,
    current: impl Fn() -> Result<(), String>,
) -> Result<Vec<u8>, String> {
    if declared_length == 0 {
        return Err("请选择非空的投影文件。".into());
    }
    usize::try_from(declared_length)
        .ok()
        .filter(|length| *length <= isize::MAX as usize)
        .ok_or("文件超出了此平台可寻址的内存。")?;
    // Size the allocation from bytes read rather than stale or sparse-file metadata.
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 64 * 1_024];
    loop {
        current()?;
        let count = match reader.read(&mut chunk) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("Unable to read the file: {error}")),
        };
        if count == 0 {
            break;
        }
        bytes
            .try_reserve(count)
            .map_err(|_| "There is not enough memory to read this file.".to_string())?;
        bytes.extend_from_slice(&chunk[..count]);
    }
    if bytes.is_empty() {
        return Err("请选择非空的投影文件。".into());
    }
    Ok(bytes)
}
