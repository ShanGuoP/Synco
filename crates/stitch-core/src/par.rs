//! 分核执行：这几处只用 `std::thread::scope` 按行/列切块，不依赖 rayon 的 API
//! （imageproc 自己会带 rayon，那是它的调度）。
//! 24MP 级的模糊与重采样是纯带宽活，单核跑不到方案的 <20ms 档。

use std::thread;

fn thread_count(units: usize) -> usize {
    let avail = thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    units.max(1).min(avail)
}

/// 把 0..units 切成连续块分给各核；`units` 小于 2 万时不值得起线程
pub fn par_for_each<F>(units: usize, f: F)
where
    F: Fn(usize) + Sync,
{
    if units < 20_000 {
        (0..units).for_each(|i| f(i));
        return;
    }
    let nt = thread_count(units);
    let chunk = units.div_ceil(nt);
    let f = &f;
    thread::scope(|s| {
        let mut start = 0;
        while start < units {
            let end = (start + chunk).min(units);
            s.spawn(move || (start..end).for_each(|i| f(i)));
            start = end;
        }
    });
}

/// 按连续块切分 mutable 切片并行处理，回调收到 (块, 块序号)。
/// 行主序的图像缓冲就靠它做分核——每块独占一段内存，不需要 unsafe。
pub fn par_chunks_mut<T, F>(data: &mut [T], chunk_elems: usize, f: F)
where
    T: Send,
    F: Fn(&mut [T], usize) + Sync,
{
    let chunk = chunk_elems.max(1);
    let nchunks = data.len().div_ceil(chunk);
    if data.len() < chunk * 2 || nchunks < 2 {
        let mut i = 0;
        for blk in data.chunks_mut(chunk) {
            f(blk, i);
            i += 1;
        }
        return;
    }
    let nt = thread_count(nchunks);
    let per = nchunks.div_ceil(nt);
    let f = &f;
    // 切块必须发生在 scope 之外：scope 内的局部量活不过它的闭包体，spawn 借用不到
    let mut parts: Vec<&mut [T]> = data.chunks_mut(chunk).collect();
    thread::scope(|s| {
        for (gi, grp) in parts.chunks_mut(per).enumerate() {
            let base = gi * per;
            s.spawn(move || {
                for (k, blk) in grp.iter_mut().enumerate() {
                    f(blk, base + k);
                }
            });
        }
    });
}

/// 分块归约：用于色彩统计这类"每块局部求和再合并"的循环
pub fn par_reduce<F, G, T>(units: usize, identity: T, body: F, mut merge: G) -> T
where
    F: Fn(usize, usize) -> T + Sync,
    G: FnMut(T, T) -> T,
    T: Send,
{
    if units < 20_000 {
        return merge(identity, body(0, units));
    }
    let nt = thread_count(units);
    let chunk = units.div_ceil(nt);
    let body = &body;
    let parts = thread::scope(|s| {
        let mut handles = Vec::new();
        let mut start = 0;
        while start < units {
            let end = (start + chunk).min(units);
            handles.push(s.spawn(move || body(start, end)));
            start = end;
        }
        let mut acc: Vec<T> = Vec::with_capacity(handles.len());
        for hd in handles {
            match hd.join() {
                Ok(v) => acc.push(v),
                Err(_) => panic!("归约线程崩溃"),
            }
        }
        acc
    });
    parts.into_iter().fold(identity, |a, b| merge(a, b))
}
