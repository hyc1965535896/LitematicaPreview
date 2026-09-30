<div align="center">
  <img src="Assets/icon.png" alt="Litematica Preview 图标" width="200"/>
  <h1>Litematica Preview</h1>
  <p><strong>适用于 Windows 的离线 Minecraft 投影与结构查看器</strong></p>
</div>

<!-- README-I18N:START -->

[English](./README.md) | **中文**

<!-- README-I18N:END -->

> [!TIP]
> **本 `cn` 分支**(本仓库默认分支)在上游基础上提供：完整简体中文界面、材料清单按方块替换、
> 替换后导出新原理图，以及图形化选块器(软件渲染的等距方块图标，支持中文搜索)。
> 构建方式与上游相同(`scripts/build.ps1`)。上传游时请保留 l10n/ 目录的翻译对照。

Litematica Preview 是一款适用于 Windows 桌面的 Minecraft 投影与结构查看器，改编自 [LitematicaQL](https://github.com/Arcadi4/LitematicaQL)。它支持在本地以 3D 方式预览 `.litematic`、`.schem`、`.schematic`、`.nbt`、`.snbt`、`.mcstructure` 和 `.nusn` 文件。

## 安装

从 [发布页面](https://github.com/Arcadi4/LitematicaPreview/releases) 下载最新版本：

- **安装包 (`LitematicaPreview-<version>-win-x64-setup.exe`)**：按当前用户安装，无需管理员权限，并可注册所选的文件关联。
- **便携版 (`LitematicaPreview-<version>-win-x64-portable.zip`)**：解压压缩包并运行 `LitematicaPreview.exe`。请将 `Assets`、`Demos` 和 `Licenses` 目录保留在可执行文件同级目录下。

需要 Windows 10 或 11 (x64) 以及 [Microsoft Edge WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/)（Windows 11 和当前版本的 Windows 10 已预装；若缺失，安装程序会自动下载）。

> [!NOTE]
> 若需在安装后更改文件关联，请打开右上角菜单并选择 **Set as default app…** 或 **Remove file associations**。便携版也可以在 PowerShell 中通过 `.\LitematicaPreview.exe --register` 和 `.\LitematicaPreview.exe --unregister` 注册或取消注册。

## 支持的格式

| 扩展名 | 文件格式 |
| --- | --- |
| `.litematic` | Litematica |
| `.schem` | Sponge schematic（海绵投影） |
| `.schematic` | MCEdit |
| `.nbt` | Java 版结构方块（Structure block） |
| `.snbt` | 结构 SNBT（支持花括号或方括号方块状态） |
| `.mcstructure` | 基岩版结构（Bedrock structure） |
| `.nusn` | Nucleation 快照（Nucleation snapshot） |

## 开发

在开发模式下运行桌面应用：

```powershell
pnpm --prefix App install --frozen-lockfile
pnpm --prefix App exec tauri dev
```

运行前端和 Rust 检查：

```powershell
pnpm --prefix App run build
cargo test --manifest-path Mesher/Cargo.toml --release --locked
cargo test --manifest-path App/src-tauri/Cargo.toml --release --locked
```

### 构建

- Node.js 24 LTS 与 pnpm
- Rust 稳定版工具链 (`x86_64-pc-windows-msvc`)
- Visual Studio C++ Build Tools（使用 C++ 的桌面开发、x64 MSVC、Windows SDK）

```powershell
git clone https://github.com/Arcadi4/LitematicaPreview.git
cd LitematicaPreview

rustup target add x86_64-pc-windows-msvc
./scripts/build.ps1
```

构建产物（`*-setup.exe`、`*-portable.zip` 以及可直接运行的 `win-x64/` 目录）会输出到 `artifacts/`。

## 致谢

非常感谢 [@Nano112](https://github.com/Nano112) 的 [Nucleation](https://github.com/Schem-at/Nucleation) 项目为解析与网格生成管线提供支持，以及感谢 [LitematicaQL](https://github.com/Arcadi4/LitematicaQL) 提供最初的 macOS 实现。
