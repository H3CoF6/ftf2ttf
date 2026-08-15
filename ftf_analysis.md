# 腾讯魔改 TTF（FTF 变体）格式技术分析

> 本文档说明腾讯 ETFont 引擎使用的魔改 TTF（下文简称 FTF）文件格式的实现细节，
> 面向需要编写解析器 / 转换器 / 渲染器的开发者。不含逆向过程。
> 数据来源：`20125.ttf`（14873 字形，head.unitsPerEm = 256）。

## 1. 概述

FTF 是一种"寄生"在标准 SFNT 容器里的字体格式：

- 标准表（head / maxp / hhea / hmtx / cmap / name / post / OS/2）仍然存在，但 **glyf 表被掏空**，
  只剩 4 字节占位 `30 30 30 30`（"0000"），不再保存真实轮廓。
- 真实轮廓数据存放在两个自定义表中：
  - `FTFH`（FTF Header，32 字节）：版本、字形数、坐标模式等元数据。
  - `FTFG`（FTF Glyphs）：全部字形的实际数据。
- **`loca` 表被复用为 FTFG 的索引**：`loca[i]` 到 `loca[i+1]` 之间的 FTFG 字节即第 i 个字形的数据。

因此 FTF 既能被旧工具识别为"合法 TTF"（容器结构完整），又能由自定义引擎渲染真实字形。

## 2. 容器与表结构

### 2.1 表目录

`OS/2, cmap, glyf, head, hhea, hmtx, loca, maxp, name, post, FTFG, FTFH`

| 表 | 说明 |
|---|---|
| `FTFH` | 32 字节头部元数据 |
| `FTFG` | 全部字形数据（20125.ttf 中 476762 字节） |
| `loca` | 格式 1（u32 BE 偏移），numGlyphs+1 项，直接索引 FTFG |
| `glyf` | 4 字节占位 `30 30 30 30`，可忽略 / 删除 |

### 2.2 FTFH（32 字节）

| 偏移 | 大小 | 字段 | 说明 |
|---|---|---|---|
| 0 | u32 BE | version | 恒为 0x00010000 |
| 4 | u32 BE | numGlyphs | 字形总数 |
| 8 | u16 BE | unitScale | 恒等于 1 << shift |
| 10 | u8 | shift | 坐标缩放，20125.ttf 为 8（1<<8 = 256，对应 upem=256） |
| 11 | u8 | flags | 高 4 位 = mode1，低 4 位 = mode2 |
| 12 | u16 BE | reserved | 引擎内部使用，转换可忽略 |
| 14 | u16 BE | reserved | 同上 |

坐标模式（mode1 / mode2）：

- mode == 1：坐标用 **1 字节有符号数**（int8）
- mode != 1：坐标用 **2 字节有符号大端数**（int16 BE）

mode1 用于变换矩阵系数，mode2 用于点坐标与平移量 tx/ty。

## 3. FTFG 字形数据

每个字形 = `loca` 切片出的一段字节 = **一串子记录（sub-record）**。

子记录分两类，由首字节 bit7 区分：

- bit7 = 1：**简单轮廓记录**（0x80 族）
- bit7 = 0：**组件引用记录**

两类记录的首字节 bit6 均为"还有后续子记录"标志。

### 3.1 简单轮廓记录（0x80 族）

```
byte0    : 0x80 | flags        （bit6 = 还有后续子记录）
byte1    : count1              (u8)
byte2-3  : count2              (u16 BE)
[坐标区]   total 个点，每点按 mode2 读 x 再读 y
[标志区]   total 个字节，每点 1 字节
[附加区]   仅当 count1 > 0 时存在，count2 字节（语义待补充）
```

- 总点数 total = count1 + count2。20125.ttf 中 count1 恒为 0（头部固定 4 字节）。
- mode2=1：每点 2 字节 `x: i8, y: i8`；mode2=2：每点 4 字节 `x: i16 BE, y: i16 BE`。
- 点标志字节：
  - bit0：on-curve（1 = 曲线上的点，0 = 二次贝塞尔控制点）
  - bit7：contour-end（该点是当前闭合轮廓的最后一个点）
  - 其余位：本字体未使用
- 语义与 TrueType glyf 一致：点序列 + on/off 标志即可重建标准轮廓。

### 3.2 组件引用记录

```
byte0        : flags | (gidLen & 7)      （bit6 = 还有后续子记录，低 3 位 = gid 字节数）
byte1..gidLen: gid（大端，1~7 字节）
[变换描述符]
```

- 语义：当前字形的轮廓 = 目标字形 gid 的轮廓，先完成目标字形自身的变换，再套用本记录的矩阵。
- 引用可以嵌套（目标字形本身也可以是组件引用），需递归展开并做环检测。

### 3.3 变换描述符

```
byte0 : flags
[可选值，按 flags 位依次出现]
```

| 位 | 含义 | 默认值 | 读取模式 | 备注 |
|---|---|---|---|---|
| bit0 | sx | 64 | mode1 | |
| bit1 | kx | 0 | mode1 | |
| bit2 | ky | 0 | mode1 | |
| bit3 | sy | 64 | mode1 | |
| bit4 | tx | 0 | mode2 | 读取后 <<6 |
| bit5 | ty | 0 | mode2 | 读取后 <<6 |
| bit6 | 额外值 | — | mode1 | 用途未知 |

矩阵系数为 26.6 定点数（64 = 1.0），平移量 tx/ty 读取后左移 6 位。

点变换（行向量 (x, y) 乘矩阵 M = [[sx, kx], [ky, sy]]，再右移 6 位）：

```
x' = (sx·x + ky·y + tx) >> 6
y' = (kx·x + sy·y + ty) >> 6
```

### 3.4 组合语义

设子记录序列为 R1, R2, ...，字形最终轮廓 = 所有子记录展开结果的并集。

对组件引用 R（目标 gid = g，矩阵 = M）：

1. 递归展开 g，得到其原始轮廓点集 P
2. 输出 = { M·p | p ∈ P }

即先应用子字形自身的变换，再应用当前记录矩阵（逐层向外套用）。简单记录则直接输出其点集。

## 4. 坐标系统

FTF 内部坐标是**以字形中心为原点的有符号整数**，配合 shift 缩放（范围约 -128..127）。

引擎输出到 TrueType 空间时做一次平移和 y 翻转：

```
x_out = x_raw + 128
y_out = 92 - y_raw
```

- 平移 128：把以 0 为中心的坐标搬进 [0, 256]（upem=256）
- y 翻转：FTF 的 y 轴屏幕朝下，TrueType 的 y 轴基线朝上

该映射经全字形验证：所有字形 bbox 严格落在 head 声明的 (-70, -98, 300, 284) 内。

> 注意：偏移量（128, 92）来自 20125.ttf 的引擎配置，其他字体 / 版本可能不同，
> 建议用 head bbox 或已知字形反推。

## 5. 指标数据（hmtx）

原文件 hmtx 与 maxp 不一致（内部缺陷）：

- numberOfHMetrics = 8144
- hmtx 实际只含 8144 组 (advance, lsb) + 161 个 lsb-only 条目
- 剩余约 6528 个字形的度量缺失，按 TrueType 规则继承最后一个 advance（=256）

标准解析器（如 fontTools）直接反编译会报错。转换时需按原始字节自行解析，
输出时补齐全部字形度量。

## 6. 转换为标准 TTF 的算法

1. 解析 SFNT 表目录，读取 FTFH / FTFG / loca / hhea / hmtx / head / maxp。
2. 校验 FTFH 版本（0x00010000），读取 numGlyphs / mode1 / mode2。
3. 对每个 gid：取 loca[gid]..loca[gid+1] 的 FTFG 切片，按第 3 节递归解析，
   得到 (点集, 标志) 列表；组件引用逐层应用矩阵。
4. 应用第 4 节坐标映射，得到输出空间轮廓。
5. 重建 glyf：每个子记录按点标志 bit7 切分轮廓，写入 endPtsOfContours，
   on-curve 标志写入点标志数组。
6. 重建 loca（格式 1）、hmtx（补齐度量，lsb = xMin）、head（xMin..yMax = 全字形 bbox，
   indexToLocFormat = 1）、maxp（maxPoints / maxContours）。
7. 删除 FTFG / FTFH / 旧 glyf / 旧 loca，重新编译保存为标准 TTF。

## 7. 20125.ttf 数据统计

| 项目 | 值 |
|---|---|
| numGlyphs | 14873 |
| 空字形（无轮廓） | 5971 |
| 简单记录（0x80 族） | 1317 |
| 组件引用记录 | 7585 |
| cmap 映射码点 | 7136 |
| head.unitsPerEm | 256 |
| head bbox | (-70, -98, 300, 284) |

## 8. 通用性注意事项

- mode1 / mode2 决定坐标宽度，解析器应按 FTFH 动态选择，不要写死。
- count1 > 0 的简单记录带附加区，本字体未出现，字段语义待补充。
- 坐标偏移 (128, 92) 可能随字体变化，建议用 head bbox 校验。
- 组件引用 gid 可达 7 字节，按首字节低 3 位动态读取。