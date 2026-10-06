//! 尺寸档位折算：长边上限、按 16 对齐、超出比例时的取舍都在这一个口径里。
//! 接口方硬约束：宽高都是 16 的倍数、长边 ≤3840、比例 ≤3:1、总像素 655360 ~ 3840×2160。

pub const EDGE_MAX: u32 = 3840;
pub const PX_MAX: u64 = 3840 * 2160;
pub const PX_MIN: u64 = 655_360;
pub const RATIO_MAX: f64 = 3.0;

fn f16(n: f64) -> u32 {
    ((n / 16.0).floor() * 16.0).max(16.0) as u32
}

fn c16(n: f64) -> u32 {
    ((n / 16.0).ceil() * 16.0).max(16.0) as u32
}

/// `'1024x1024'` / `'1536'` / `' 2048 '` 都取长边；读不出按 1024
pub fn long_edge(text: &str) -> u32 {
    if let Some(n) = pair_long_edge(text) {
        if n >= 256 {
            return n;
        }
    }
    match int_prefix(text) {
        Some(n) if n >= 256 => n,
        _ => 1024,
    }
}

/// 手写扫描替代 `(\d{2,5})\s*[x×*]\s*(\d{2,5})`，内核不引正则依赖
fn pair_long_edge(text: &str) -> Option<u32> {
    let b: Vec<char> = text.chars().collect();
    let run = |i: usize| -> Option<(usize, u32)> {
        let mut j = i;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j - i >= 2 && j - i <= 5 {
            let s: String = b[i..j].iter().collect();
            Some((j, s.parse().unwrap()))
        } else {
            None
        }
    };
    let skip_ws = |mut i: usize| {
        while i < b.len() && b[i].is_whitespace() {
            i += 1;
        }
        i
    };
    let mut i = 0;
    while i < b.len() {
        if let Some((mut j, a)) = run(i) {
            j = skip_ws(j);
            if j < b.len() && matches!(b[j], 'x' | 'X' | '×' | '*') {
                if let Some((_, c)) = run(skip_ws(j + 1)) {
                    return Some(a.max(c));
                }
            }
        }
        i += 1;
    }
    None
}

/// `parseInt(str, 10)`：前导空白/符号后取数字，取不到就是 None（对应 NaN）
fn int_prefix(text: &str) -> Option<u32> {
    let t = text.trim_start();
    let (neg, rest) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let mut v: u32 = 0;
    let mut any = false;
    for ch in rest.chars() {
        match ch.to_digit(10) {
            Some(d) => {
                v = v.saturating_mul(10).saturating_add(d);
                any = true;
            }
            None => break,
        }
    }
    if !any {
        return None;
    }
    // JS 那边 n>=256 的门槛在调用处判，负数取绝对值会让 '-2048' 通过，这里保持 NaN 语义
    if neg {
        None
    } else {
        Some(v)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fit {
    pub w: u32,
    pub h: u32,
    pub unfit: bool,
}

/// 把源尺寸折成接口合法的尺寸。
/// `edge` 是目标长边（设置里的字符串先过 `long_edge`）；比例不可能分毫不差，
/// 在下取整 / 上取整四种组合里挑比例误差最小的，再按像素上下限校正。
pub fn fit_size(w: u32, h: u32, edge: u32) -> Fit {
    fit_size_with_cap(w, h, edge.min(EDGE_MAX) as f64)
}

/// `edge` 传设置原文，等价 `fitSize(cw, ch, edge)`
pub fn fit_size_from_setting(w: u32, h: u32, edge: &str) -> Fit {
    fit_size(w, h, long_edge(edge))
}

fn fit_size_with_cap(w: u32, h: u32, cap: f64) -> Fit {
    let (wf, hf) = (w as f64, h as f64);
    let area = w as u64 * h as u64;
    let src = wf / hf;

    // pick(k, minPx)：四组合里取比例误差最小；误差相等时先出现的赢（Set 的插入序 = 下取整在前）
    let pick = |k: f64, min_px: u64| -> Option<(u32, u32, f64)> {
        let dedup = |a: u32, b: u32| -> Vec<u32> {
            if a == b {
                vec![a]
            } else {
                vec![a, b]
            }
        };
        let ws = dedup(f16(wf * k), c16(wf * k));
        let hs = dedup(f16(hf * k), c16(hf * k));
        let mut best: Option<(u32, u32, f64)> = None;
        for &ww in &ws {
            for &hh in &hs {
                let a = ww as u64 * hh as u64;
                if ww < 16 || hh < 16 || ww.max(hh) > EDGE_MAX || a > PX_MAX || a < min_px {
                    continue;
                }
                let err = ((ww as f64 / hh as f64) - src).abs() / src;
                if best.map(|b| err < b.2).unwrap_or(true) {
                    best = Some((ww, hh, err));
                }
            }
        }
        best
    };

    let k = 1.0f64
        .min(cap / (w.max(h) as f64))
        .min(((PX_MAX as f64) / area as f64).sqrt());

    let mut s = pick(k, 0);
    if s.map(|(_, ww, hh)| (ww as u64 * hh as u64) < PX_MIN).unwrap_or(true) {
        let up = pick(k.max((PX_MIN as f64 / area as f64).sqrt()), PX_MIN);
        if up.is_some() {
            s = up;
        }
    }
    let (sw, sh) = match s {
        Some((ww, hh, _)) => (ww, hh),
        // 顶破长边时 pick 返回空，保持原样并交给 unfit 判定
        None => (f16(wf * k).max(16), f16(hf * k).max(16)),
    };
    let a = sw as u64 * sh as u64;
    Fit {
        w: sw,
        h: sh,
        unfit: (sw.max(sh) as f64 / sw.min(sh) as f64) > RATIO_MAX
            || a < PX_MIN
            || sw.max(sh) > EDGE_MAX
            || a > PX_MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_edge_三种写法() {
        assert_eq!(long_edge("1024x1024"), 1024);
        assert_eq!(long_edge("1536"), 1536);
        assert_eq!(long_edge(" 2048 "), 2048);
        assert_eq!(long_edge("2048×1365"), 2048);
        assert_eq!(long_edge("1024*512"), 1024);
        assert_eq!(long_edge(""), 1024);
        assert_eq!(long_edge("64"), 1024);
        assert_eq!(long_edge("auto"), 1024);
    }

    #[test]
    fn fit_size_竖拍_24mp_折到合法档() {
        let f = fit_size(4000, 6000, 3840);
        assert_eq!((f.w, f.h), (2352, 3520));
        assert!(!f.unfit);
    }

    #[test]
    fn fit_size_两边都是16的倍数且不超上限() {
        for (w, h) in [(4000u32, 6000u32), (6000, 4000), (3000, 3000), (1024, 1024), (900, 1600)] {
            let f = fit_size(w, h, 1024);
            assert_eq!(f.w % 16, 0, "{w}x{h} -> {}", f.w);
            assert_eq!(f.h % 16, 0);
            assert!(f.w.max(f.h) <= EDGE_MAX);
            assert!((f.w as u64 * f.h as u64) <= PX_MAX);
        }
    }

    #[test]
    fn fit_size_小裁切区被像素下限顶上去() {
        // 200x200 只有 4 万像素，低于 PX_MIN，重选后必须过下限
        let f = fit_size(200, 200, 1024);
        assert_eq!(f.w, f.h);
        assert!((f.w as u64 * f.h as u64) >= PX_MIN || f.unfit);
    }

    #[test]
    fn fit_size_比例过扁判_unfit() {
        let f = fit_size(3900, 1000, 1024);
        assert!(f.unfit);
    }
}
