# 生成字体子集字表：GB2312 全表（6763 汉字 + 符号区）+ ASCII + 中西文常用标点。
# 用法：python tools/make-font-chars.py  →  tools/font-chars.txt
# 字表覆盖日常文本（文件名、报错文案）99%+；字表外生僻字由浏览器回退系统字体。
import sys

chars = set()

# GB2312 汉字区：高字节 B0-F7，低字节 A1-FE（一级 3755 + 二级 3008）
for hi in range(0xB0, 0xF8):
    for lo in range(0xA1, 0xFF):
        try:
            chars.add(bytes([hi, lo]).decode('gb2312'))
        except UnicodeDecodeError:
            pass

# GB2312 符号区：A1-A9 行（全角标点、制表符、希腊/西里尔字母、拼音注音等）
for hi in range(0xA1, 0xAA):
    for lo in range(0xA1, 0xFF):
        try:
            chars.add(bytes([hi, lo]).decode('gb2312'))
        except UnicodeDecodeError:
            pass

# ASCII 可打印区
chars.update(chr(c) for c in range(0x20, 0x7F))

# CJK 标点与全角形式
for cp in list(range(0x3000, 0x3040)) + list(range(0xFF00, 0xFFF0)):
    chars.add(chr(cp))

# 散点补字：省略号/破折号引号已含于上，这里补几个 GB2312 外但常用的
for ch in '〇··——‘’“”…、。〈〉《》「」『』【】〔〕！（），．：；？～￥％×÷±°′″℃℉€£§¶†‡•※→←↑↓■□▲△▼◆○●★☆♀♂':
    chars.add(ch)

out = sys.argv[1] if len(sys.argv) > 1 else 'tools/font-chars.txt'
with open(out, 'w', encoding='utf-8') as f:
    f.write(''.join(sorted(chars)))
print(f'{out}: {len(chars)} 字符')
