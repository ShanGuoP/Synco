// 调整参数默认形状与面板字段清单，字段名与服务端 EditOps 一致。
import { blankPortrait } from '../portrait.js';
import { blankDevelop } from './developModel.js';
/** 与后端 photoedit-core 的 EditOps 同形：缺的字段服务端按默认补，多余字段会被拒 */
export const blankOps = () => ({
  v: 1,
  geometry: { crop: null, rotate_deg: 0, flip_h: false, flip_v: false, fill: 'edge' },
  warp: { strokes: [], auto: { face_slim: 0, eye_big: 0, nose_slim: 0, chin: 0 } },
  color: { exposure: 0, contrast: 0, highlights: 0, shadows: 0, temp: 0, tint: 0, saturation: 0, vibrance: 0, clarity: 0, sharpen: 0, preset: null },
  beauty: { smooth: 0, texture: 0, blemish: 0, even_tone: 0, brighten: 0, de_shine: 0, sharpen: 0, by_mask: false, auto_face: false },
  portrait: blankPortrait(),
  lut: null,
  develop: blankDevelop(),
});

export const RATIOS = [['free', 'ad.ratioFree', 0], ['1:1', 'ad.ratio11', 1], ['4:3', 'ad.ratio43', 4 / 3], ['3:4', 'ad.ratio34', 3 / 4], ['16:9', 'ad.ratio169', 16 / 9], ['3:2', 'ad.ratio32', 1.5], ['9:16', 'ad.ratio916', 9 / 16]];
export const COLOR_KEYS = [
  ['exposure', 'ad.exposure', 'ad.exposureTip'],
  ['contrast', 'ad.contrast', 'ad.contrastTip'],
  ['highlights', 'ad.highlights', 'ad.highlightsTip'],
  ['shadows', 'ad.shadows', 'ad.shadowsTip'],
  ['temp', 'ad.temp', 'ad.tempTip'],
  ['tint', 'ad.tint', 'ad.tintTip'],
  ['saturation', 'ad.saturation', 'ad.saturationTip'],
  ['vibrance', 'ad.vibrance', 'ad.vibranceTip'],
  ['clarity', 'ad.clarity', 'ad.clarityTip'],
  ['sharpen', 'ad.sharpen', 'ad.sharpenTip'],
];
export const BEAUTY_KEYS = [
  ['smooth', 'ad.smooth', 'ad.smoothTip'],
  ['texture', 'ad.texture', 'ad.textureTip'],
  ['blemish', 'ad.blemish', 'ad.blemishTip'],
  ['even_tone', 'ad.evenTone', 'ad.evenToneTip'],
  ['brighten', 'ad.brighten', 'ad.brightenTip'],
  ['de_shine', 'ad.deShine', 'ad.deShineTip'],
  ['sharpen', 'ad.sharpen', 'ad.beautySharpenTip'],
];
export const BRUSHES = [['push', 'ad.push', 'ad.pushTip'], ['pucker', 'ad.pucker', 'ad.puckerTip'], ['bloat', 'ad.bloat', 'ad.bloatTip'], ['restore', 'ad.restore', 'ad.restoreTip']];
