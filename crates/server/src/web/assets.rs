//! 发布态把 `public/` 打进 exe。
//!
//! 页面加载的是本机 http 地址，HTML/JS/字体必须由这个服务给——`/api/...`
//! 这类相对请求才能落在同一个源上。开发态 rust-embed 走读盘，改完刷新就见效；
//! `SYNCO_PUBLIC` 指了别的目录也以那边为准。
//!
//! 资源表本身（`Public`）在 `crate::text`：那个 derive 一处只能有一份，
//! 服务端取文案与这里取文件走的是同一个嵌入。

use crate::text::Public;

/// 磁盘上读不到时的兜底：发布态界面就在 exe 里。
/// 返回形状与 `files::stat_and_read` 一致（ETag + 内容），调用方不用分两种来源写。
pub fn read(rel: &str, inm: Option<&str>) -> Option<(String, Option<Vec<u8>>)> {
    let hit = Public::get(rel.trim_start_matches('/'))?;
    // 内嵌内容跟着 exe 走，没有 mtime 可看，用长度当判新依据就够了
    let etag = format!("\"embed-{:x}\"", hit.data.len());
    if inm == Some(etag.as_str()) {
        return Some((etag, None));
    }
    Some((etag, Some(hit.data.into_owned())))
}



#[cfg(test)]
mod tests {
    #[test]
    fn 入口页在资源表里() {
        // 取不到就说明 folder 路径或打包配置错了，界面会整片空白
        assert!(crate::text::Public::get("index.html").map(|f| f.data.len() > 200).unwrap_or(false));
    }
}
