# v0.3.0-cn1

基于上游 v0.3.0 的中文版。

## Features

- 完整简体中文界面：主界面、菜单、设置、对话框、安装器与错误信息全部汉化。
- 材料清单支持按方块名称替换（调色板级替换，保留原方块的属性；替换语义参考 DimasKama 的 [SchematicPreview](https://github.com/DimasKama/SchematicPreview) 模组），替换后可导出新的 .litematic / .schem / .nbt / .snbt / .mcstructure 等格式。
- 图形化选块器：替换对话框改为游戏内 Litematica 风格的图标网格，全部 1106 种方块各自渲染等距 3D 图标（软件渲染，含草方块/树叶/水染色、动画贴图取帧、楼梯台阶等形状）；支持中文名或 id 搜索，悬浮提示显示"中文名 + minecraft:id"。
- 空气以线框立方体呈现并保持可选，作为"删除方块"的替换目标（技术性的 cave_air / void_air 除外）。
- 方块中文名对照（1088 个）提取自 Minecraft 26.x 官方 zh_cn 语言文件。

## Improvements

- 材料列表每行显示方块图标与中文名，替换规则以芯片形式展示并可单独移除。
- l10n/ 目录保留翻译对照表与套用脚本（apply-l10n.mjs），便于跟随上游更新重新汉化。

## ToDos

- 同上游 v0.3.0（最近打开的文件历史、光照着色器）。

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
