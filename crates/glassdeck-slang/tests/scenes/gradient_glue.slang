import glassdeck;
import gradient;

struct GdInputs {
    float3 night; float3 violet; float3 rose; float3 teal;
    float2 roseAt; float2 tealAt; float2 glowAt;
};

uniform GdInputs gd_inputs;

Inputs gd_load() {
    Inputs i;
    i.night = half3(gd_inputs.night);
    i.violet = half3(gd_inputs.violet);
    i.rose = half3(gd_inputs.rose);
    i.teal = half3(gd_inputs.teal);
    i.roseAt = gd_inputs.roseAt;
    i.tealAt = gd_inputs.tealAt;
    i.glowAt = gd_inputs.glowAt;
    return i;
}

[shader("fragment")]
float4 gd_fragment(float4 at : SV_Position) : SV_Target {
    Pixel p;
    p.uv = at.xy / frame.resolution;
    p.position = p.uv * frame.panel;
    return float4(gradient(p, gd_load()));
}
