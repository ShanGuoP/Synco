// 蒙版 PNG 编码 worker：4000×6000 的编码是主线程最重的一笔（空载 ~100ms，
// GPU 被 ComfyUI 占着时数倍），挪到这里输入完全不受影响
self.onmessage = async (e) => {
  const { id, full } = e.data;
  try {
    const f = new OffscreenCanvas(full.width, full.height);
    f.getContext('2d').drawImage(full, 0, 0);
    const blob = await f.convertToBlob({ type: 'image/png' });
    self.postMessage({ id, blob });
  } catch (err) {
    self.postMessage({ id, error: String((err && err.message) || err) });
  } finally {
    full.close();
  }
};
