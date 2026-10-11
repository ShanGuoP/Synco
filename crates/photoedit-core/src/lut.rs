//! LUT：`.cube` 文本的解析与应用（1D 与 3D）。
//!
//! 3D 用三线性插值，插值前先按 `DOMAIN_MIN/MAX` 把输入搬回 0..1——
//! 很多导出工具会把域写成非 0..1，忽视这一步会让整张图偏色。

use px_core::{par::par_chunks_mut, Rgba};

/// 一张已解析的 LUT
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Lut {
    dim: Dim,
    size: usize,
    min: [f32; 3],
    max: [f32; 3],
    /// 1D 是 `size*3`，3D 是 `size³*3`
    data: Vec<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Dim {
    One,
    Three,
}

/// `.cube` 解析失败的位置。核心不知道语言包长什么样，只交出一把钥匙与几个数——
/// 句子由字典给（服务端把它塞进 `srv.adjust.lutParse` 的 `{detail}` 参数里，中英各查各的）。
#[derive(Debug, Clone)]
pub struct CubeError {
    pub code: &'static str,
    pub args: serde_json::Value,
}

impl CubeError {
    fn of(code: &'static str, args: serde_json::Value) -> Self {
        CubeError { code, args }
    }

    /// 某一行的问题：行号是这句里唯一的参数
    fn line(code: &'static str, n: usize) -> Self {
        Self::of(code, serde_json::json!({ "line": n + 1 }))
    }

    /// 整份文件的问题（缺声明、域写反）
    fn whole(code: &'static str) -> Self {
        Self::of(code, serde_json::json!({}))
    }
}

impl std::fmt::Display for CubeError {
    /// 日志与控制台看的是"钥匙 + 参数"：核心这一层没有语言可判，拼句子要等到界面或服务端
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.code, self.args)
    }
}

/// 解析 `.cube` 文本。只认标准关键字，其余行（注释、空行、自定义头部）忽略；
/// 数值残缺或尺寸对不上直接报错，不做"尽力而为"的半张表。
pub fn parse_cube(src: &str) -> Result<Lut, CubeError> {
    let mut dim = None;
    let mut size = 0usize;
    let mut min = [0.0f32, 0.0, 0.0];
    let mut max = [1.0f32, 1.0, 1.0];
    let mut data: Vec<f32> = Vec::new();
    for (n, line) in src.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let (head, rest) = match t.find(|c: char| c == ' ' || c == '\t') {
            Some(i) => (&t[..i], t[i..].trim()),
            None => (t, ""),
        };
        match head.to_ascii_uppercase().as_str() {
            "TITLE" => {}
            "DOMAIN_MIN" => min = triple(rest).ok_or_else(|| CubeError::line("lut.domainMin", n))?,
            "DOMAIN_MAX" => max = triple(rest).ok_or_else(|| CubeError::line("lut.domainMax", n))?,
            "LUT_1D_SIZE" => {
                size = parse_size(rest, n)?;
                dim = Some(Dim::One);
            }
            "LUT_3D_SIZE" => {
                size = parse_size(rest, n)?;
                dim = Some(Dim::Three);
            }
            _ => {
                // 其余行只可能是数据行："r g b" 三个 0..1 的浮点
                if let Some(v) = triple(t) {
                    data.extend_from_slice(&v);
                }
            }
        }
        if let Some(d) = dim {
            let need = match d {
                Dim::One => size * 3,
                Dim::Three => size * size * size * 3,
            };
            if data.len() > need {
                return Err(CubeError::line("lut.overflow", n));
            }
        }
    }
    let dim = dim.ok_or_else(|| CubeError::whole("lut.noSize"))?;
    let need = match dim {
        Dim::One => size * 3,
        Dim::Three => size * size * size * 3,
    };
    if data.len() != need {
        return Err(CubeError::of("lut.fewComponents", serde_json::json!({ "got": data.len(), "need": need })));
    }
    if !(min[0] < max[0] && min[1] < max[1] && min[2] < max[2]) {
        return Err(CubeError::whole("lut.domainOrder"));
    }
    Ok(Lut { dim, size, min, max, data })
}

fn parse_size(rest: &str, n: usize) -> Result<usize, CubeError> {
    let v: usize = rest.trim().parse().map_err(|_| CubeError::line("lut.sizeNotInt", n))?;
    if !(2..=256).contains(&v) {
        return Err(CubeError::of("lut.sizeRange", serde_json::json!({ "line": n + 1, "value": v })));
    }
    Ok(v)
}

fn triple(s: &str) -> Option<[f32; 3]> {
    let mut it = s.split_whitespace();
    let a = it.next()?.parse().ok()?;
    let b = it.next()?.parse().ok()?;
    let c = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some([a, b, c])
}

impl Lut {
    pub fn size(&self) -> usize {
        self.size
    }

    pub fn is_3d(&self) -> bool {
        self.dim == Dim::Three
    }

    #[inline]
    fn at(&self, i: usize) -> [f32; 3] {
        [self.data[i * 3], self.data[i * 3 + 1], self.data[i * 3 + 2]]
    }

    /// 单个颜色（0..255）过表，返回 0..255 浮点。越界输入夹进定义域。
    /// `.cube` 的 DOMAIN 说的是**归一化**输入，所以先除以 255 再搬进定义域。
    pub fn map(&self, r: f32, g: f32, b: f32) -> [f32; 3] {
        let n = |v: f32, k: usize| {
            let lo = self.min[k];
            let hi = self.max[k];
            (v / 255.0 - lo).clamp(0.0, 1.0) / (hi - lo)
        };
        match self.dim {
            Dim::One => {
                let pick = |v: f32, k: usize| {
                    let f = v * (self.size - 1) as f32;
                    let i0 = f.floor() as usize;
                    let i1 = (i0 + 1).min(self.size - 1);
                    let a = self.data[i0 * 3 + k];
                    let b2 = self.data[i1 * 3 + k];
                    a + (b2 - a) * (f - i0 as f32)
                };
                [pick(n(r, 0), 0) * 255.0, pick(n(g, 1), 1) * 255.0, pick(n(b, 2), 2) * 255.0]
            }
            Dim::Three => {
                let s = self.size;
                let f = [n(r, 0) * (s - 1) as f32, n(g, 1) * (s - 1) as f32, n(b, 2) * (s - 1) as f32];
                let i = f.map(|v| v.floor() as usize);
                let d = f.map(|v| v - v.floor());
                let idx = |x: usize, y: usize, z: usize| (z * s + y) * s + x;
                let c000 = self.at(idx(i[0], i[1], i[2]));
                let c100 = self.at(idx((i[0] + 1).min(s - 1), i[1], i[2]));
                let c010 = self.at(idx(i[0], (i[1] + 1).min(s - 1), i[2]));
                let c110 = self.at(idx((i[0] + 1).min(s - 1), (i[1] + 1).min(s - 1), i[2]));
                let c001 = self.at(idx(i[0], i[1], (i[2] + 1).min(s - 1)));
                let c101 = self.at(idx((i[0] + 1).min(s - 1), i[1], (i[2] + 1).min(s - 1)));
                let c011 = self.at(idx(i[0], (i[1] + 1).min(s - 1), (i[2] + 1).min(s - 1)));
                let c111 = self.at(idx((i[0] + 1).min(s - 1), (i[1] + 1).min(s - 1), (i[2] + 1).min(s - 1)));
                let mut out = [0f32; 3];
                for k in 0..3 {
                    let x0 = c000[k] + (c100[k] - c000[k]) * d[0];
                    let x1 = c010[k] + (c110[k] - c010[k]) * d[0];
                    let y0 = x0 + (x1 - x0) * d[1];
                    let x0 = c001[k] + (c101[k] - c001[k]) * d[0];
                    let x1 = c011[k] + (c111[k] - c011[k]) * d[0];
                    let y1 = x0 + (x1 - x0) * d[1];
                    out[k] = (y0 + (y1 - y0) * d[2]) * 255.0;
                }
                out
            }
        }
    }
}

/// 应用 LUT，`strength` 0..1 是与原图的混合比。1.0 = 完全过表。
pub fn apply(img: &Rgba, lut: &Lut, strength: f32) -> Rgba {
    if strength <= 0.0 {
        return img.clone();
    }
    let mut out = Rgba::new(img.w, img.h);
    let rows = img.h.div_ceil(16).max(1);
    let src = &img.px;
    let w = img.w;
    par_chunks_mut(&mut out.px, rows * w * 4, |blk, band| {
        let y0 = band * rows;
        for (i, px) in blk.chunks_exact_mut(4).enumerate() {
            let g = (y0 * w + i) * 4;
            let [r, gc, b] = lut.map(src[g] as f32, src[g + 1] as f32, src[g + 2] as f32);
            let mix = |a: u8, v: f32| (a as f32 * (1.0 - strength) + v.clamp(0.0, 255.0) * strength).round().clamp(0.0, 255.0) as u8;
            px[0] = mix(src[g], r);
            px[1] = mix(src[g + 1], gc);
            px[2] = mix(src[g + 2], b);
            px[3] = src[g + 3];
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 恒等 3D 表：每个格点就是自己的归一化颜色
    fn identity3d(size: usize) -> String {
        let mut s = format!("LUT_3D_SIZE {size}\n");
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    s.push_str(&format!("{} {} {}\n", r as f32 / (size - 1) as f32, g as f32 / (size - 1) as f32, b as f32 / (size - 1) as f32));
                }
            }
        }
        s
    }

    #[test]
    fn 恒等表过完不变() {
        let lut = parse_cube(&identity3d(17)).unwrap();
        let img = px_core::Rgba::new(8, 4);
        let mut img = img;
        for (i, p) in img.px.chunks_exact_mut(4).enumerate() {
            let v = i as u32;
            p.copy_from_slice(&[(v * 31) as u8, (v * 7 + 11) as u8, (v * 53 % 256) as u8, 255]);
        }
        let out = apply(&img, &lut, 1.0);
        for i in 0..32 {
            // 17³ 的网格对 8bit 输入最多差半档
            for c in 0..3 {
                let d = (out.px[i * 4 + c] as i32 - img.px[i * 4 + c] as i32).abs();
                assert!(d <= 2, "第 {i} 个像素通道 {c} 差 {d}");
            }
        }
    }

    #[test]
    fn 强度为零就是原图() {
        let lut = parse_cube(&identity3d(4)).unwrap();
        let mut img = px_core::Rgba::new(2, 1);
        img.px.copy_from_slice(&[10, 200, 40, 255, 250, 3, 77, 128]);
        assert_eq!(apply(&img, &lut, 0.0), img);
    }

    #[test]
    fn 解析最小合法表() {
        let src = "TITLE \"x\"\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
        let t = "LUT_3D_SIZE 2\n".to_string() + src;
        let lut = parse_cube(&t).unwrap();
        assert!(lut.is_3d());
        assert_eq!(lut.size(), 2);
        assert_eq!(lut.map(255.0, 0.0, 255.0), [255.0, 0.0, 255.0]);
    }

    #[test]
    fn 一d表按通道各查各() {
        let t = "LUT_1D_SIZE 2\n0 0 0\n1 1 1\n";
        let lut = parse_cube(t).unwrap();
        assert!(!lut.is_3d());
        let m = lut.map(128.0, 128.0, 128.0);
        assert!((m[0] - 128.0).abs() < 0.02, "{m:?}");
        // 通道各自独立：G 的第二个分量写成 0.5，就只有 G 会折半
        let lut = parse_cube("LUT_1D_SIZE 2\n0 0 0\n1 0.5 1\n").unwrap();
        let m = lut.map(255.0, 255.0, 0.0);
        assert_eq!(m[0], 255.0);
        assert_eq!(m[1], 127.5);
        assert_eq!(m[2], 0.0);
    }

    #[test]
    fn 自定义定义域被搬回单位区间() {
        // 域写成 0..2：输入 255（归一化 1.0）落在域的中点，对应表的第二个格点
        let t = "DOMAIN_MIN 0.0 0.0 0.0\nDOMAIN_MAX 2.0 2.0 2.0\nLUT_1D_SIZE 3\n0.0 0.0 0.0\n0.5 0.5 0.5\n1.0 1.0 1.0\n";
        let lut = parse_cube(t).unwrap();
        let m = lut.map(255.0, 0.0, 510.0_f32.min(255.0));
        assert!((m[0] - 127.5).abs() < 0.5, "{m:?}");
        assert_eq!(m[1], 0.0);
    }

    #[test]
    fn 坏表逐条报错而不是出半张表() {
        assert!(parse_cube("0 0 0\n1 1 1\n").is_err(), "没声明尺寸");
        assert!(parse_cube("LUT_3D_SIZE 2\n1 1 1\n").is_err(), "数据不够");
        assert!(parse_cube("LUT_3D_SIZE 0\n").is_err(), "尺寸为 0");
        assert!(parse_cube("LUT_3D_SIZE abc\n").is_err(), "尺寸不是整数");
        assert!(parse_cube("DOMAIN_MIN 1.0 1.0 1.0\nDOMAIN_MAX 1.0 1.0 1.0\nLUT_1D_SIZE 2\n0 0 0\n1 1 1\n").is_err(), "定义域退化成点");
        assert!(parse_cube("LUT_3D_SIZE 2\n1 1 1\n2 2 2\n3 3 3\n4 4 4\n5 5 5\n6 6 6\n7 7 7\n8 8 8\n9 9 9\n").is_err(), "数据超出声明");
    }

    #[test]
    fn 反色表把黑变白() {
        // 2³ 的反色表：每个格点取补
        let mut t = String::from("LUT_3D_SIZE 2\n");
        for b in (0..2).rev() {
            for g in (0..2).rev() {
                for r in (0..2).rev() {
                    t.push_str(&format!("{r} {g} {b}\n"));
                }
            }
        }
        let lut = parse_cube(&t).unwrap();
        let m = lut.map(0.0, 0.0, 0.0);
        assert_eq!(m, [255.0, 255.0, 255.0]);
        let m = lut.map(255.0, 255.0, 255.0);
        assert_eq!(m, [0.0, 0.0, 0.0]);
    }
}
