import glassdeck;

static const int BLOBS = 7;

// The blobs' summed falloff: 1 at a blob's edge, rising inside, so nearby blobs merge.
float field(float2 q, float t) {
    float sum = 0.0;
    for (int i = 0; i < BLOBS; i++) {
        float n = float(i);
        float2 centre = float2(
            0.89 + 0.78 * sin(t * (0.11 + 0.023 * n) + n * 1.7),
            0.5 + 0.44 * sin(t * (0.09 + 0.031 * n) + n * 2.3));
        float radius = 0.12 + 0.035 * sin(n * 3.1);
        float2 d = q - centre;
        sum += radius * radius / max(dot(d, d), 1e-4);
    }
    return sum;
}

[Canvas]
half4 lava(Pixel p) {
    float2 q = float2(p.uv.x * frame.resolution.x / frame.resolution.y, p.uv.y);
    float f = field(q, frame.time);
    float body = smoothstep(0.92, 1.08, f);
    float rim = smoothstep(0.45, 1.0, f) * (1.0 - body);

    half3 deep = lerp(half3(0.09h, 0.02h, 0.12h), half3(0.03h, 0.01h, 0.06h), half(p.uv.y));
    half3 hot = lerp(half3(1.0h, 0.55h, 0.12h), half3(0.95h, 0.12h, 0.38h), half(p.uv.y));
    half3 c = deep + hot * half(saturate(f * 0.12));
    c = lerp(c, hot * 0.55h, half(rim * 0.8));
    c = lerp(c, hot * half(0.85 + 0.15 * saturate(f - 1.0)), half(body));
    return half4(dither(c, p), 1);
}
