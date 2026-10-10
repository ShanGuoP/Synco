//! 内核只处理像素数组：图走 RGBA，蒙版只用 alpha 一条通道。
//! 蒙版画布只有一层 alpha：所有蒙版运算（外扩、羽化、判新）读的都是 alpha，RGB 只在色彩校正与融合里用到。

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alpha {
    pub w: usize,
    pub h: usize,
    pub v: Vec<u8>,
}

/// 闭开区间，和 `inkBBox` 返回的 `{x0,y0,x1,y1}` 一致
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Box2 {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rgba {
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, px: vec![0u8; w * h * 4] }
    }

    pub fn from_pixels(w: usize, h: usize, px: Vec<u8>) -> Self {
        assert_eq!(px.len(), w * h * 4, "RGBA 字节数与尺寸不符");
        Self { w, h, px }
    }

    #[inline]
    pub fn off(&self, x: usize, y: usize) -> usize {
        (y * self.w + x) * 4
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> [u8; 4] {
        let i = self.off(x, y);
        [self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]]
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, p: [u8; 4]) {
        let i = self.off(x, y);
        self.px[i..i + 4].copy_from_slice(&p);
    }

    pub fn alpha(&self) -> Alpha {
        let mut a = Alpha::new(self.w, self.h);
        for (o, v) in a.v.iter_mut().enumerate() {
            *v = self.px[o * 4 + 3];
        }
        a
    }

    /// 把 alpha 平面还原成参与融合的画布：RGB 全白，只有 alpha 有意义。
    /// 蒙版画布的 RGB 从不被读，重采样时也不该影响 alpha 曲线。
    pub fn from_alpha(a: &Alpha) -> Self {
        let mut img = Self::new(a.w, a.h);
        for (i, &v) in a.v.iter().enumerate() {
            img.px[i * 4] = 255;
            img.px[i * 4 + 1] = 255;
            img.px[i * 4 + 2] = 255;
            img.px[i * 4 + 3] = v;
        }
        img
    }

    pub fn same_size(&self, other: &Rgba) -> bool {
        self.w == other.w && self.h == other.h
    }
}

impl Alpha {
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, v: vec![0u8; w * h] }
    }

    pub fn from_vec(w: usize, h: usize, v: Vec<u8>) -> Self {
        assert_eq!(v.len(), w * h, "alpha 字节数与尺寸不符");
        Self { w, h, v }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> u8 {
        self.v[y * self.w + x]
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, a: u8) {
        self.v[y * self.w + x] = a;
    }

    pub fn max(&self) -> u8 {
        self.v.iter().copied().max().unwrap_or(0)
    }
}

impl Box2 {
    pub fn new(x: usize, y: usize, w: usize, h: usize) -> Self {
        Self { x, y, w, h }
    }

    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
}
