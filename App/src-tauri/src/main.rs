#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod associations;
mod formats;
mod preview;
mod preview_process;
mod protocol;
mod resources;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use preview::PreviewWorker;
use resources::{Demo, Resources};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

pub use formats::EXTENSIONS;

struct HostState {
    worker: Arc<PreviewWorker>,
    resources: Resources,
    initial_path: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Bootstrap {
    extensions: &'static [&'static str],
    demos: Vec<Demo>,
    initial_path: Option<String>,
    version: &'static str,
    request_id: u64,
    max_worker_threads: u8,
}

#[tauri::command]
async fn bootstrap(state: State<'_, HostState>) -> Result<Bootstrap, String> {
    let resources = state.resources.clone();
    let initial_path = state.initial_path.clone();
    let request_id = state.worker.begin_session();
    tauri::async_runtime::spawn_blocking(move || {
        Ok(Bootstrap {
            extensions: EXTENSIONS,
            demos: resources.demos()?,
            initial_path,
            version: env!("CARGO_PKG_VERSION"),
            request_id,
            max_worker_threads: litematica_preview_native::max_worker_threads(),
        })
    })
    .await
    .map_err(|e| format!("无法加载欢迎界面：{e}"))?
}

#[tauri::command]
async fn choose_file(app: AppHandle, window: WebviewWindow) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let extensions: Vec<_> = EXTENSIONS.iter().map(|extension| &extension[1..]).collect();
        app.dialog()
            .file()
            .set_parent(&window)
            .set_title("打开 Minecraft 投影文件")
            .add_filter("Minecraft 投影文件", &extensions)
            .add_filter("所有文件", &["*"])
            .blocking_pick_file()
            .map(|file| {
                let path = file
                    .into_path()
                    .map_err(|e| format!("无法打开所选文件：{e}"))?;
                resources::path_string(&path)
            })
            .transpose()
    })
    .await
    .map_err(|e| format!("无法显示文件选择器：{e}"))?
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewProgress {
    request_id: u64,
    phase: &'static str,
    completed: u64,
    total: u64,
}

fn preview_path(path: String) -> Result<PathBuf, String> {
    let path = PathBuf::from(path);
    let supported = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            EXTENSIONS
                .iter()
                .any(|candidate| candidate[1..].eq_ignore_ascii_case(extension))
        });
    if !supported {
        return Err(format!(
            "请选择受支持的投影文件：{}。",
            EXTENSIONS.join(", ")
        ));
    }
    Ok(path)
}

fn report_progress(
    app: &AppHandle,
    worker: &PreviewWorker,
    request_id: u64,
    completed: u64,
    total: u64,
) {
    if worker.ensure_current(request_id).is_ok() {
        let _ = app.emit(
            "preview-progress",
            PreviewProgress {
                request_id,
                phase: "mesh",
                completed,
                total,
            },
        );
    }
}

#[tauri::command]
async fn load_preview(
    path: String,
    request_id: u64,
    app: AppHandle,
    options: preview::LoadOptions,
    state: State<'_, HostState>,
) -> Result<protocol::Metadata, String> {
    let (options, replacements) = options.validate()?;
    let worker = Arc::clone(&state.worker);
    worker.advance(request_id);
    worker.ensure_current(request_id)?;
    let path = preview_path(path)?;
    let resources = state.resources.clone();
    tauri::async_runtime::spawn_blocking(move || {
        worker.ensure_current(request_id)?;
        let pack_path = resources.pack()?;
        let metadata = worker.load(
            &path,
            &pack_path,
            request_id,
            options,
            &replacements,
            |completed, total| report_progress(&app, &worker, request_id, completed, total),
        )?;
        worker.ensure_current(request_id)?;
        Ok(metadata)
    })
    .await
    .map_err(|e| format!("预览工作进程意外停止：{e}"))?
}

#[tauri::command]
fn start_preview(
    path: String,
    request_id: u64,
    app: AppHandle,
    options: preview::LoadOptions,
    state: State<'_, HostState>,
) -> Result<(), String> {
    let (options, replacements) = options.validate()?;
    if options.thread_count.is_none() {
        return Err("流式预览需要启用多线程。".into());
    }
    let path = preview_path(path)?;
    let worker = Arc::clone(&state.worker);
    worker.advance(request_id);
    worker.begin_stream(request_id, options)?;
    let resources = state.resources.clone();
    tauri::async_runtime::spawn(async move {
        let producer = Arc::clone(&worker);
        let result = tauri::async_runtime::spawn_blocking(move || {
            let pack = resources.pack()?;
            producer.load_stream(
                &path,
                &pack,
                request_id,
                options,
                &replacements,
                |completed, total| {
                    report_progress(&app, &producer, request_id, completed, total)
                },
            )
        })
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => worker.fail_stream(request_id, error),
            Err(error) => worker.fail_stream(
                request_id,
                format!("预览工作进程意外停止：{error}"),
            ),
        }
    });
    Ok(())
}

#[tauri::command]
async fn next_preview(
    request_id: u64,
    previous_batch_id: Option<u64>,
    state: State<'_, HostState>,
) -> Result<protocol::StreamEvent, String> {
    let worker = Arc::clone(&state.worker);
    tauri::async_runtime::spawn_blocking(move || worker.next(request_id, previous_batch_id))
        .await
        .map_err(|e| format!("无法接收下一个预览批次：{e}"))?
}

#[tauri::command]
async fn read_preview(
    request_id: u64,
    batch_id: Option<u64>,
    ranges: Vec<protocol::ReadRange>,
    state: State<'_, HostState>,
) -> Result<tauri::ipc::Response, String> {
    let worker = Arc::clone(&state.worker);
    tauri::async_runtime::spawn_blocking(move || {
        worker
            .read(request_id, batch_id, &ranges)
            .map(tauri::ipc::Response::new)
    })
    .await
    .map_err(|e| format!("无法读取预览缓冲区：{e}"))?
}

#[tauri::command]
fn preview_memory(request_id: u64, state: State<'_, HostState>) -> Option<u64> {
    state.worker.preview_memory(request_id)
}

#[tauri::command]
fn release_preview(request_id: u64, state: State<'_, HostState>) {
    state.worker.release(request_id);
}

#[tauri::command]
fn cancel_load(request_id: u64, state: State<'_, HostState>) {
    state.worker.advance(request_id);
}

#[tauri::command]
async fn register_associations() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        associations::register()?;
        associations::open_settings()
    })
    .await
    .map_err(|e| format!("无法注册文件关联：{e}"))?
}

#[tauri::command]
async fn unregister_associations() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(associations::unregister)
        .await
        .map_err(|e| format!("无法移除文件关联：{e}"))?
}

#[tauri::command]
async fn show_licenses(state: State<'_, HostState>) -> Result<(), String> {
    let resources = state.resources.clone();
    tauri::async_runtime::spawn_blocking(move || open_folder(&resources.licenses()?))
        .await
        .map_err(|e| format!("无法打开许可证文件夹：{e}"))?
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportOutcome {
    destination: String,
    replaced: i64,
    block_count: i64,
}

#[tauri::command]
async fn export_schematic(
    app: AppHandle,
    window: WebviewWindow,
    path: String,
    format: String,
    replacements: Vec<litematica_preview_native::BlockReplacement>,
    state: State<'_, HostState>,
) -> Result<ExportOutcome, String> {
    litematica_preview_native::validate_replacements(&replacements)?;
    let export_format = litematica_preview_native::ExportFormat::from_extension(&format)
        .ok_or_else(|| format!("不受支持的导出格式：{format}"))?;
    let path = preview_path(path)?;
    let default_name = format!(
        "{}-已替换.{}",
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("schematic"),
        export_format.extension()
    );
    let destination = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_parent(&window)
            .set_title("导出新原理图")
            .set_file_name(&default_name)
            .add_filter("Minecraft 投影文件", &[export_format.extension()])
            .blocking_save_file()
            .map(|file| {
                file.into_path()
                    .map_err(|e| format!("无法打开所选文件：{e}"))
            })
            .transpose()
    })
    .await
    .map_err(|e| format!("无法显示保存对话框：{e}"))?
    .map_err(|e| format!("无法打开所选文件：{e}"))?;
    let Some(destination) = destination else {
        return Err("Cancelled".into());
    };
    let outcome_destination = destination.clone();
    let worker = Arc::clone(&state.worker);
    tauri::async_runtime::spawn_blocking(move || {
        worker.export(&path, &replacements, &format, &destination, None)
    })
    .await
    .map_err(|e| format!("预览工作进程意外停止：{e}"))?
    .map(|summary| ExportOutcome {
        destination: resources::path_string(&outcome_destination).unwrap_or_default(),
        replaced: summary.replaced,
        block_count: summary.block_count,
    })
}

#[tauri::command]
async fn export_materials(
    app: AppHandle,
    window: WebviewWindow,
    path: String,
    replacements: Vec<litematica_preview_native::BlockReplacement>,
    state: State<'_, HostState>,
) -> Result<ExportOutcome, String> {
    litematica_preview_native::validate_replacements(&replacements)?;
    let path = preview_path(path)?;
    let default_name = format!(
        "{}-materials.xlsx",
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("schematic")
    );
    let destination = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_parent(&window)
            .set_title("导出材料清单")
            .set_file_name(&default_name)
            .add_filter("Excel 工作簿", &["xlsx"])
            .blocking_save_file()
            .map(|file| {
                file.into_path()
                    .map_err(|e| format!("无法打开所选文件：{e}"))
            })
            .transpose()
    })
    .await
    .map_err(|e| format!("无法显示保存对话框：{e}"))?
    .map_err(|e| format!("无法打开所选文件：{e}"))?;
    let Some(destination) = destination else {
        return Err("Cancelled".into());
    };
    let outcome_destination = destination.clone();
    let worker = Arc::clone(&state.worker);
    tauri::async_runtime::spawn_blocking(move || {
        worker.export(&path, &replacements, "materials", &destination, None)
    })
    .await
    .map_err(|e| format!("预览工作进程意外停止：{e}"))?
    .map(|summary| ExportOutcome {
        destination: resources::path_string(&outcome_destination).unwrap_or_default(),
        replaced: summary.replaced,
        block_count: summary.block_count,
    })
}

#[tauri::command]
async fn block_catalog(state: State<'_, HostState>) -> Result<Vec<String>, String> {
    let resources = state.resources.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let pack_path = resources.pack()?;
        block_catalog_from_pack(&pack_path)
    })
    .await
    .map_err(|e| format!("无法读取方块列表：{e}"))?
}

/// Icons come from the bundled pack, fixed for the process lifetime.
static BLOCK_ICONS: std::sync::OnceLock<Result<Arc<[litematica_preview_native::BlockIcon]>, String>> =
    std::sync::OnceLock::new();

#[tauri::command]
async fn block_icons(
    state: State<'_, HostState>,
) -> Result<Arc<[litematica_preview_native::BlockIcon]>, String> {
    if let Some(cached) = BLOCK_ICONS.get() {
        return match cached {
            Ok(icons) => Ok(Arc::clone(icons)),
            Err(message) => Err(message.clone()),
        };
    }
    let resources = state.resources.clone();
    tauri::async_runtime::spawn_blocking(move || {
        BLOCK_ICONS
            .get_or_init(|| {
                let pack_path = resources.pack()?;
                let bytes = std::fs::read(&pack_path)
                    .map_err(|e| format!("无法读取随附的方块资源：{e}"))?;
                litematica_preview_native::block_icons(&bytes)
                    .map(Arc::from)
                    .map_err(|e| format!("无法生成方块图标：{e}"))
            })
            .clone()
    })
    .await
    .map_err(|e| format!("无法生成方块图标：{e}"))?
}

/// Enumerates block ids from the bundled resource pack's blockstate files.
fn block_catalog_from_pack(pack_path: &Path) -> Result<Vec<String>, String> {
    let file = std::fs::File::open(pack_path).map_err(|e| format!("无法打开随附的方块资源：{e}"))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("无法读取随附的方块资源：{e}"))?;
    let mut names = std::collections::BTreeSet::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|e| format!("无法读取随附的方块资源：{e}"))?;
        let name = entry.name();
        let Some(rest) = name.strip_prefix("assets/") else {
            continue;
        };
        let Some((namespace, tail)) = rest.split_once('/') else {
            continue;
        };
        let Some(relative) = tail.strip_prefix("blockstates/") else {
            continue;
        };
        let Some(block) = relative.strip_suffix(".json") else {
            continue;
        };
        if block.is_empty() || namespace.is_empty() {
            continue;
        }
        names.insert(format!("{namespace}:{block}"));
    }
    Ok(names.into_iter().collect())
}

#[cfg(windows)]
fn open_folder(path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let folder: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let open: Vec<u16> = "open\0".encode_utf16().collect();
    // Pass the directory as a separate UTF-16 argument. ShellExecute does not
    // execute an interpolated command, and resource paths may contain spaces.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            open.as_ptr(),
            folder.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if result as isize <= 32 {
        return Err(format!(
            "Windows 无法打开许可证文件夹（错误代码 {}）。",
            result as isize
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn open_folder(_: &std::path::Path) -> Result<(), String> {
    Err("Litematica Preview 的桌面集成仅支持 Windows。".into())
}

fn initial_path() -> Result<Option<String>, String> {
    let mut args = std::env::args_os().skip(1);
    let Some(argument) = args.next() else {
        return Ok(None);
    };
    if args.next().is_some() {
        return Err("每个窗口只能打开一个投影文件。".into());
    }
    let path = PathBuf::from(argument);
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|e| format!("无法解析投影文件路径：{e}"))?
            .join(path)
    };
    resources::path_string(&absolute).map(Some)
}

fn run() -> Result<(), String> {
    let initial_path = initial_path()?;
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let resources = Resources::new(app.path().resource_dir()?);
            app.manage(HostState {
                worker: Arc::new(PreviewWorker::default()),
                resources,
                initial_path,
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::Destroyed) {
                window.state::<HostState>().worker.advance(u64::MAX);
            }
        })
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            choose_file,
            load_preview,
            start_preview,
            next_preview,
            read_preview,
            preview_memory,
            release_preview,
            cancel_load,
            register_associations,
            unregister_associations,
            show_licenses,
            export_schematic,
            block_catalog,
            block_icons,
            export_materials,
        ])
        .run(tauri::generate_context!())
        .map_err(|e| format!("无法启动 Litematica Preview：{e}"))
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    // Decoder mode bypasses Tauri and WebView2. It receives authentication
    // through the inherited stdin pipe, not process arguments or a Tauri command.
    if first
        .as_deref()
        .is_some_and(|argument| argument == "--preview-worker")
    {
        let result = match (args.next(), args.next()) {
            (Some(port), None) => preview_process::run(&port),
            _ => Err("解码进程参数无效。".into()),
        };
        if let Err(error) = result {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    let remaining: Vec<_> = args.collect();
    let registration = match first.as_deref().and_then(|arg| arg.to_str()) {
        Some("--register") if remaining.is_empty() => Some(associations::register()),
        Some("--unregister") if remaining.is_empty() => Some(associations::unregister()),
        Some("--default-apps") if remaining.is_empty() => Some(associations::open_settings()),
        Some("--register-extensions") => Some(match remaining.as_slice() {
            [selection] => selection
                .to_str()
                .ok_or_else(|| "扩展名选择无效。".to_string())
                .and_then(associations::register_extensions),
            _ => Err("请传入以逗号分隔的受支持扩展名列表。".into()),
        }),
        Some("--register" | "--unregister" | "--default-apps") => {
            Some(Err("注册参数不符合预期。".into()))
        }
        _ => None,
    };
    // Installer hooks must exit before Tauri, WebView2, or the Settings dialog
    // starts; NSIS reports the nonzero process status.
    if let Some(result) = registration {
        if let Err(error) = result {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    if let Err(error) = run() {
        eprintln!("{error}");
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
            let message: Vec<u16> = error.encode_utf16().chain(Some(0)).collect();
            let title: Vec<u16> = "Litematica Preview\0".encode_utf16().collect();
            unsafe {
                MessageBoxW(
                    std::ptr::null_mut(),
                    message.as_ptr(),
                    title.as_ptr(),
                    MB_OK | MB_ICONERROR,
                );
            }
        }
        std::process::exit(1);
    }
}
