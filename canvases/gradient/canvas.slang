import glassdeck;

[Inputs]
struct Inputs {
    half3 night; half3 violet; half3 rose; half3 teal;
    float2 roseAt; float2 tealAt; float2 glowAt;
}

[Canvas]
half4 gradient(Pixel p, Inputs i) {
    float2 q = float2(p.uv.x * frame.resolution.x / frame.resolution.y, p.uv.y);
    half3 c = lerp(i.night, i.violet, half(p.uv.y));
    c = lerp(c, i.rose, bump(q, i.roseAt, 0.9) * 0.75h);
    c = lerp(c, i.teal, bump(q, i.tealAt, 1.0) * 0.7h);
    c = lerp(c, i.violet * 1.5h, bump(q, i.glowAt, 0.75) * 0.6h);
    return half4(dither(c, p), 1);
}
