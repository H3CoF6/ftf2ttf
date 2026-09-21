# FTF2TTF

高性能QQ魔改 FTF 字体转标准 TTF 转换器

## 简介

FTF2TTF 是一个用 Rust 编写的命令行工具，用于将腾讯修改过的 FTF 格式字体文件转换回标准的 TTF 字体文件。

QQ的**部分字体**使用了自定义的格式（在 TTF 基础上添加了 FTFH 和 FTFG 表），这会导致字体在其他应用中无法正常使用。本工具可以将这些字体还原为标准格式。

## 功能特性

- [x] 自动检测并跳过已经是标准 TTF 的文件
- [x] 支持单文件转换和批量转换
- [x] 支持递归目录扫描
- [x] 并行处理，转换速度快
- [x] 自动生成输出文件名（`原文件名_result.ttf`）
- [x] 完整的错误处理和友好的提示信息
- [x] 可选输出**彩色字体**：把私有 `brsh` / `cglf` 表编译成标准 `COLR` v1 + `CPAL` v0（含逐组选色）
- [x] 可选导出**内嵌图片序列**：把私有 `eimg` 表里的 PNG 帧写到磁盘
- [x] 可选用本地 **OTS（OpenType Sanitizer）** 预检产物，不用等浏览器报错

## 安装

### 从源码构建

确保已安装 Rust 工具链（推荐使用 [rustup](https://rustup.rs/)）

```bash
git clone https://github.com/yourusername/ftf2ttf.git
cd ftf2ttf
cargo build --release
```

编译后的可执行文件位于 `target/release/ftf2ttf`

## 使用方法

### 基本用法

```bash
# 转换单个文件（自动命名为 input_result.ttf）
ftf2ttf input.ttf

# 指定输出文件名
ftf2ttf input.ttf -o output.ttf

# 批量转换目录（输出到 converted 子目录）
ftf2ttf ./fonts

# 批量转换并指定输出目录
ftf2ttf ./fonts -o ./output

# 递归扫描子目录
ftf2ttf ./fonts -r
```

### 命令行参数

```
Usage: ftf2ttf [OPTIONS] <INPUT>

Arguments:
  <INPUT>  输入 FTF 字体文件或目录

Options:
  -o, --output <OUTPUT>    输出 TTF 字体文件或目录
  -r, --recursive          递归转换目录（批量模式）
  -c, --color              输出彩色字体（COLR v1 + CPAL v0）
      --color-chars <CHARS> 额外强制上色的字符（QQ 皮肤的逐字清单不在字体里）
      --dump-assets <DIR>  把 eimg 内嵌 PNG 帧序列导出到目录
      --check-ots          用本地 ots-sanitize 校验产物
  -h, --help               显示帮助信息
```

### 彩色字体与图片导出

QQ 的部分字体在 FTF 之上还挂了私有彩色表：

- `brsh`：颜料表（单色或线性渐变色带），颜色全部在这里；
- `cglf`：**逐组选笔表**（已解析，见下文）；
- `eimg`：炫彩/场景字体里内嵌的多帧 PNG 动画素材。

```bash
# 生成彩色 TTF：@font-face 挂上即可显示彩色（Chromium/Electron 原生支持 COLRv1）
ftf2ttf 23161.ttf -c -o 23161-color.ttf

# 导出 eimg 里的图片序列：<DIR>/<字体名>/frame_000.png ...
ftf2ttf 20405.ttf --dump-assets ./assets -o 20405.ttf

# 54981 的「想生联合狩猎塔罗之」不在表里，靠参数补上，得到与 QQ 一致的结果
ftf2ttf 54981.ttf -c --color-chars '想生联合狩猎塔罗之' -o 54981-color.ttf
```

> QQ 皮肤里还有一小撮**零散汉字**会被上色（如 54981 的 `想生联合狩猎塔罗之`）。
> 这份逐字清单**不在字体文件里**：`name`（无自定义记录）、`post`、`cglf`
> （`想` 与同组 68 个字共用同一条零记录）、`csty`/`assy`/`sgrp`（空表或 2 条记录）、
> `brsh`（3 支笔）、`FTFH`（32B 头）、字形轮廓（与普通字无差别）都已排查，
> 整文件里也搜不到这 9 个字的 UTF-16/UTF-32/UTF-8 或 gid 清单，所以它属于客户端/皮肤侧。
> 用 `--color-chars` 传入即可 1:1 复现。

彩色输出会丢弃 `brsh` / `cglf` / `eimg` 等私有表（它们已转换成标准表或与静态字形无关）。

**`cglf` 结构（实测 54981 / 23161）**：

```text
16B  头  { u32 version=0x00010000, u32 numGlyphs, u32 0x0000FF00, u32 1 }
u16[numGlyphs]        每字形所属「组」编号（按 gid 连续分块，值域 0..maxGroup）
50B  子头            25 × u16，值恒等于 maxGroup
组记录[maxGroup 条]  每组 8B：{ u16 0x4000, u16 笔刷<<8, u16 0x4000, u16 笔刷<<8 }
```

记录全 0 ⇒ 该组不上色。这与 QQ 的实际显示对得上：54981 只有组 0..45 非零
（= gid 0..101，正好是 `0-9 A-Z a-z` + ASCII 标点 + `¡¢£`），笔刷 0 = 红 `#BF3E1E`；
CJK 组全 0 ⇒ 汉字不上色。记录布局识别不出来（例如 20405 的组值带高位标志）时，
回退到「有轮廓就上色」+ 按 GID 轮转的近似策略。

### 使用示例

**示例 1：转换单个文件**
```bash
$ ftf2ttf 20563.ttf
Converting "20563.ttf" -> "20563_result.ttf"
Done in 45.23ms
```

**示例 2：跳过正常 TTF**
```bash
$ ftf2ttf normal_font.ttf
Skipping "normal_font.ttf": already a normal TTF
Done in 2.15ms
```

**示例 3：批量转换**
```bash
$ ftf2ttf ./fonts -r
Found 15 files to convert...
Converted: "font1.ttf"
Converted: "font2.ttf"
Skipping "font3.ttf": already a normal TTF
...
Batch conversion finished in 2.34s
```

## 技术原理

FTF 格式是腾讯在标准 TTF 基础上添加了自定义表的修改版本：

- **FTFH 表**：存储字形数量、版本信息和编码模式
- **FTFG 表**：存储压缩/变换后的字形数据

本工具的转换过程：

1. 解析 FTFH 表获取元数据
2. 从 FTFG 表还原字形轮廓数据
3. 处理复合字形和变换矩阵
4. 重建标准 TTF 的 glyf 和 loca 表
5. 更新 head、hhea、hmtx、maxp 等表
6. 重新计算校验和并输出标准 TTF

详细的格式分析请查看：**[ftf_analysis.md](ftf_analysis.md)**

## 测试

项目包含完整的集成测试：

```bash
# 运行所有测试
cargo test

# 只运行集成测试
cargo test --test integration_test

# 代码质量检查
cargo clippy --all-targets -- -D warnings
```

测试覆盖 `resources/` 目录下的样本文件，包括 FTF 格式和正常 TTF 格式。

### 用 OTS 预检（推荐）

浏览器加载字体前会先过一遍 OTS，失败时只会丢一句 “Failed to decode downloaded font”，
很难定位。本仓库接入了 OTS 官方命令行工具，可以在本地把错误一次性看清楚：

```bash
# 一次性安装到项目内 .venv-ots/（不污染全局环境）
./scripts/setup-ots.sh

# 对 resources/ 下所有字体跑「转换 + OTS 校验」
cargo test --test ots

# 转换时顺便校验
cargo run -- 23161.ttf --color --check-ots
```

OTS 的查找顺序：环境变量 `OTS_SANITIZE` → `PATH` 里的 `ots-sanitize` → 项目内 `.venv-ots`。
找不到时相关测试会自动跳过。

## 项目结构

```
ftf2ttf/
├── src/
│   ├── lib.rs          # 核心转换逻辑
│   ├── color.rs        # brsh/cglf -> COLRv1 + CPAL
│   ├── assets.rs       # eimg -> PNG 帧序列
│   ├── ots.rs          # 本地 ots-sanitize 调用
│   └── main.rs         # CLI 入口
├── tests/
│   ├── integration_test.rs  # 集成测试
│   └── ots.rs               # OTS 校验测试
├── scripts/
│   └── setup-ots.sh         # 安装本地 OTS
├── resources/          # 测试用字体文件
├── Cargo.toml
├── README.md
└── ftf_analysis.md     # 格式分析文档
```

## 依赖

- `clap` - 命令行参数解析
- `rayon` - 并行处理
- `anyhow` - 错误处理
- `walkdir` - 目录遍历

## 许可证

MIT 
