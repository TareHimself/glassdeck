import glassdeck;

[Inputs]
struct Inputs {
    float bass; float mid; float treble;
    float beat; float beatAge;
    float phase; float hue;
}

// What the ink was last frame, swirled and faded each frame by flow below.
[Target] [Feedback] [Scale(0.5)] [Format("rgba16f")]
Sampler2D<half4> ink;

static const int ORBS = 5;
static const float TAU = 6.2831853;

float hash(float2 q) {
    return frac(sin(dot(q, float2(127.1, 311.7))) * 43758.5453);
}

float noise(float2 q) {
    float2 cell = floor(q);
    float2 f = frac(q);
    float2 u = f * f * (3.0 - 2.0 * f);
    float a = hash(cell), b = hash(cell + float2(1, 0)), c = hash(cell + float2(0, 1)), d = hash(cell + float2(1, 1));
    return lerp(lerp(a, b, u.x), lerp(c, d, u.x), u.y);
}

// Soft colours that blend into each other as t moves.
half3 palette(float t) {
    return half3(0.5 + 0.5 * cos(TAU * (t + float3(0.0, 0.1, 0.2))));
}

[Pass(0)] [Writes("ink")]
half4 flow(Pixel p, Inputs i) {
    float aspect = frame.resolution.x / frame.resolution.y;
    float2 q = float2(p.uv.x * aspect, p.uv.y);

    // Where this pixel's ink was a frame ago: a slow swirl round the centre, a pull outward with the
    // bass, and a wander along a noise field.
    float2 d = p.uv - 0.5;
    float turn = 0.002 + 0.006 * i.bass;
    float2 swirled = float2(d.x * cos(turn) - d.y * sin(turn), d.x * sin(turn) + d.y * cos(turn));
    float2 back = 0.5 + swirled * (0.996 - 0.006 * i.bass);
    float angle = noise(q * 2.3 + i.phase * 0.05) * TAU;
    back += float2(cos(angle), sin(angle)) * (0.0012 + 0.0025 * i.mid) / float2(aspect, 1.0);
    half3 ink_then = ink.Sample(back).rgb * half(0.958 - 0.02 * i.treble);

    // Glowing orbs on slow loops, brighter with the music.
    half3 fresh = half3(0.0h);
    for (int k = 0; k < ORBS; k++) {
        float n = float(k);
        float2 at = float2(
            aspect * (0.5 + 0.38 * sin(i.phase * (0.31 + 0.07 * n) + n * 1.9)),
            0.5 + 0.36 * sin(i.phase * (0.23 + 0.05 * n) + n * 2.7));
        float radius = 0.03 + 0.05 * i.bass;
        float2 off = q - at;
        half glow = half(exp(-dot(off, off) / (radius * radius)));
        fresh += palette(i.hue + n * 0.13) * glow * half(0.06 + 0.22 * i.mid);
    }

    // A ring spreading from the centre on each beat.
    float spread = length(d * float2(aspect, 1.0));
    float ring = exp(-pow((spread - i.beatAge * 1.4) * 60.0, 2.0)) * i.beat;
    fresh += palette(i.hue + 0.5) * half(ring * 0.14);

    return half4(ink_then + fresh, 1.0h);
}

[Canvas]
half4 show(Pixel p, Inputs i) {
    half3 c = ink.Sample(p.uv).rgb;
    c = half3(1.0h) - exp(-c * 1.6h);
    half3 deep = lerp(half3(0.02h, 0.01h, 0.05h), half3(0.05h, 0.02h, 0.09h), half(p.uv.y));
    return half4(dither(max(c, deep), p), 1);
}
