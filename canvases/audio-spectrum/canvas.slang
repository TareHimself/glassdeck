import glassdeck;

static const int BANDS = 24;

[Inputs]
struct Inputs {
    float levels[24];
}

[Canvas]
[FrameRate(60)]
half4 spectrum(Pixel p, Inputs i) {
    float x = p.uv.x * float(BANDS);
    int band = min(int(x), BANDS - 1);
    float within = frac(x);
    float level = i.levels[band];
    float height = 1.0 - p.uv.y;

    float column = smoothstep(0.06, 0.12, within) * (1.0 - smoothstep(0.88, 0.94, within));
    float bar = column * (1.0 - smoothstep(level - 0.006, level + 0.006, height));
    float halo = column * level * exp(-abs(height - level) * 18.0);

    half3 low = half3(0.05h, 0.78h, 0.86h);
    half3 mid = half3(0.46h, 0.28h, 0.98h);
    half3 high = half3(1.0h, 0.28h, 0.56h);
    half h = half(height);
    half3 tint = h < 0.5h ? lerp(low, mid, h * 2.0h) : lerp(mid, high, h * 2.0h - 1.0h);

    half3 c = lerp(half3(0.01h, 0.01h, 0.03h), half3(0.05h, 0.03h, 0.11h), half(p.uv.y));
    c += tint * half(halo * 0.35);
    c = lerp(c, tint, half(bar));
    return half4(dither(c, p), 1);
}
