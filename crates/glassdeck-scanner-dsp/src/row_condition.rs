use crate::MAX_ROW_SENSORS;

/// Removes the slowly varying spatial part of one row before keys are reconstructed (Keyglass
/// `fw_row_condition.h`). Takes signed calibrated sensor deltas, left to right, and leaves them
/// with four fractional bits. A local magnetic peak is not taken as an offset the row shares.
pub fn condition_row_q4(row: &mut [i32]) {
    let n = row.len();
    if n == 0 || n > MAX_ROW_SENSORS {
        return;
    }
    let mut sorted = [0i32; MAX_ROW_SENSORS];
    sorted[..n].copy_from_slice(row);
    sorted[..n].sort_unstable();
    if sorted[0] == sorted[n - 1] {
        row.fill(0);
        return;
    }

    // Four fractional bits keep an even-length row's half-count median.
    let median_q4 = (sorted[(n - 1) / 2] + sorted[n / 2]) * 8;
    let mut fit = [median_q4; MAX_ROW_SENSORS];
    let position = |i: usize| 2 * i as i32 - (n as i32 - 1);

    let mut prior_inliers = 0u32;
    for _pass in 0..2 {
        let (mut inliers, mut count, mut sum_x, mut sum_xx) = (0u32, 0i32, 0i32, 0i32);
        let (mut sum_y, mut sum_xy) = (0i64, 0i64);
        for i in 0..n {
            let y = row[i] * 16;
            let residual = y - fit[i];
            if residual <= -100 * 16 || residual >= 100 * 16 {
                continue;
            }
            let x = position(i);
            inliers |= 1 << i;
            count += 1;
            sum_x += x;
            sum_xx += x * x;
            sum_y += y as i64;
            sum_xy += x as i64 * y as i64;
        }
        if count < 4 || inliers == prior_inliers {
            break;
        }
        prior_inliers = inliers;

        // A least-squares line about the mean inlier position. Slope and position carry 16
        // fractional bits each; shifting their product right by 32 rounds toward negative infinity.
        let denominator = count * sum_xx - sum_x * sum_x;
        let slope_q16 =
            ((count as i64 * sum_xy - sum_x as i64 * sum_y) * 65536) / denominator as i64;
        let mean_y = (sum_y / count as i64) as i32;
        let mean_x_q16 = sum_x * 65536 / count;
        for (i, f) in fit[..n].iter_mut().enumerate() {
            *f = mean_y + ((slope_q16 * (position(i) * 65536 - mean_x_q16) as i64) >> 32) as i32;
        }
    }
    for (value, f) in row.iter_mut().zip(fit) {
        *value = *value * 16 - f;
    }
}
