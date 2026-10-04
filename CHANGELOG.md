# v0.3.1

## Features

- Fix box counts assuming every item stacks to 64: counts now divide by the item's real max stack size like Litematica — minecarts, shulker boxes, boats, buckets, beds, tools and armor hold 27 per box, snowballs/eggs/signs/banners hold 432 (481 hopper minecarts now report 17.9 boxes instead of 0.3).
- Fix the side-by-side workbook writing duplicate out-of-order `<row>` numbers, which Excel "repairs" by deleting the container cells (openpyxl/WPS tolerated the disorder, Excel did not).
- Merge wall-mounted blocks into the item that places them in the material list (墙上的告示牌 → 告示牌, 墙上的红石火把 → 红石火把, wall torches, skulls, heads, banners, hanging signs), matching Litematica's own item-based list so the export works as a restocking list.
- Read container contents into the material list export. The "容器内材料种类" count and the "投影容器列表" table (container name, item name, item id, total, box count) are filled from each region's block entities, so chests, hoppers, droppers, dispensers, crafter, barrels and shulker boxes are listed with the items stored inside them. Contents are aggregated per container type and sorted by amount; a shulker box inside another container counts as one item. Item names use the client's Simplified Chinese names.

# v0.3.0

## Features

- Enable multithreaded preview generation by default with 4 worker threads when enough logical processors are available. Existing saved settings are preserved. Loading time reduced by about 50%.
- Add a conservative memory scheduling option for multithreaded previews. It is disabled by default; enabling it reduces concurrent mesh work and queued upload batches, which may slow decoding.
- Show the combined host and decoder private working sets during loading. This figure excludes WebView2 and GPU memory.

## Improvements

- Start uploading complete geometry batches while later chunks are still being generated. Keep mesh work, host batches, and upload preparation bounded, and discard staged results on cancellation or failure.
- Pack preview data into shared arenas for fewer IPC reads and GPU buffer allocations without changing draw order.
- Keep the optional decoder process-memory limit effective with either scheduling mode.

## ToDos

- Add recent-files history.
- Add lighting shader.

# v0.2.0

## Features

- Add preview settings for chunk separation and an optional decoder memory limit. The limit is off by default; when enabled, it starts
  at 2048 MB and accepts values up to 8192 MB.
- Show separate loading progress for decoding, mesh generation, and model upload.
- Display decoder memory while loading and uploaded model-data size in the footer.

## Improvements

- Replace the chunk-size selector with a five-stop slider: 16, 32, 64, 128, and 256 blocks per side.
- Reduce peak decoding memory for .litematic previews with fixed-buffer, two-pass streaming and compact block indexing. Peak memory usage dropped by **90%**.
- Avoid retaining the full decompressed .litematic document and packed block-state arrays during preview decoding.

## ToDos

- Add recent-files history.
- Add lighting shader.
- Continue improving memory use for formats that retain dense decoding paths.

# v0.1.0

> Initial release of Litematica Preview — an offline Minecraft schematic and structure viewer for Windows x64.

## Highlights

- Supports `.litematic`, `.schem`, `.schematic`, `.nbt`, `.snbt`, `.mcstructure`, and `.nusn` formats
- Fast offline rendering powered by WebGL 2 and Nucleation
- Distributed as both an installer (`.exe`) and a portable zip package

## ToDos

- Decrease memory usage when decoding schematic and structure
- Make memory limit a changeable option
- Support "history files" feature
- Add lightning shader
