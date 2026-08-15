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
  -h, --help              显示帮助信息
```

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

## 项目结构

```
ftf2ttf/
├── src/
│   ├── lib.rs          # 核心转换逻辑
│   └── main.rs         # CLI 入口
├── tests/
│   └── integration_test.rs  # 集成测试
├── resources/          # 测试用字体文件
│   ├── 20352/
│   ├── 20402/
│   ├── 20563/
│   └── 22004/
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
