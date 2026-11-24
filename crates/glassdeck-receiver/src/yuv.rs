#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Matrix {
    Bt601,
    Bt709,
}

/// Limited-range coefficients in 16.16 fixed point: luma gain, then V->R, U->G, V->G, U->B.
struct Coefficients {
    y: i32,
    vr: i32,
    ug: i32,
    vg: i32,
    ub: i32,
}

const HALF: i32 = 1 << 15;
const BT601: Coefficients = Coefficients { y: 76_284, vr: 104_595, ug: 25_624, vg: 53_281, ub: 132_186 };
const BT709: Coefficients = Coefficients { y: 76_284, vr: 117_504, ug: 13_954, vg: 34_903, ub: 138_453 };

/// An NV12 picture: a full-size luma plane, then interleaved half-size chroma, both `pitch` bytes per row.
pub struct Nv12<'a> {
    pub luma: &'a [u8],
    pub chroma: &'a [u8],
    pub width: usize,
    pub height: usize,
    pub pitch: usize,
}

pub fn nv12_to_xrgb(picture: &Nv12<'_>, matrix: Matrix, out: &mut [u32]) {
    let c = match matrix {
        Matrix::Bt601 => &BT601,
        Matrix::Bt709 => &BT709,
    };
    for row in 0..picture.height {
        let luma = &picture.luma[row * picture.pitch..][..picture.width];
        let chroma = &picture.chroma[(row / 2) * picture.pitch..][..picture.width];
        let dst = &mut out[row * picture.width..][..picture.width];
        for col in 0..picture.width {
            let y = (luma[col] as i32 - 16) * c.y;
            let u = chroma[col & !1] as i32 - 128;
            let v = chroma[col | 1] as i32 - 128;
            let r = clamp((y + c.vr * v + HALF) >> 16);
            let g = clamp((y - c.ug * u - c.vg * v + HALF) >> 16);
            let b = clamp((y + c.ub * u + HALF) >> 16);
            dst[col] = r << 16 | g << 8 | b;
        }
    }
}

fn clamp(v: i32) -> u32 {
    v.clamp(0, 255) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert_one(y: u8, u: u8, v: u8, matrix: Matrix) -> u32 {
        let luma = [y, y, y, y];
        let chroma = [u, v, u, v];
        let picture = Nv12 { luma: &luma, chroma: &chroma, width: 2, height: 2, pitch: 2 };
        let mut out = [0u32; 4];
        nv12_to_xrgb(&picture, matrix, &mut out);
        assert!(out.iter().all(|&p| p == out[0]));
        out[0]
    }

    fn channels(p: u32) -> [i32; 3] {
        [(p >> 16 & 0xFF) as i32, (p >> 8 & 0xFF) as i32, (p & 0xFF) as i32]
    }

    fn assert_near(p: u32, expected: [i32; 3]) {
        let got = channels(p);
        for (g, e) in got.iter().zip(expected) {
            assert!((g - e).abs() <= 2, "got {got:?}, expected {expected:?}");
        }
    }

    #[test]
    fn limited_range_black_and_white() {
        assert_eq!(convert_one(16, 128, 128, Matrix::Bt709), 0x00_00_00);
        assert_eq!(convert_one(235, 128, 128, Matrix::Bt709), 0xFF_FF_FF);
    }

    #[test]
    fn bt709_red_and_bt601_red_differ() {
        assert_near(convert_one(63, 102, 240, Matrix::Bt709), [255, 0, 0]);
        assert_near(convert_one(81, 90, 240, Matrix::Bt601), [255, 0, 0]);
    }

    #[test]
    fn honors_pitch_padding() {
        let luma = [235, 235, 0, 0, 235, 235, 0, 0];
        let chroma = [128, 128, 0, 0];
        let picture = Nv12 { luma: &luma, chroma: &chroma, width: 2, height: 2, pitch: 4 };
        let mut out = [0u32; 4];
        nv12_to_xrgb(&picture, Matrix::Bt709, &mut out);
        assert_eq!(out, [0xFF_FF_FF; 4]);
    }
}
