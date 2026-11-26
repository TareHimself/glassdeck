// The glassdeck Slang API, version 1.

[__AttributeUsage(_AttributeTargets.Function)]
public struct CanvasAttribute {};

[__AttributeUsage(_AttributeTargets.Function)]
public struct EffectAttribute {};

[__AttributeUsage(_AttributeTargets.Function)]
public struct PassAttribute { int order; };

[__AttributeUsage(_AttributeTargets.Var)]
public struct TargetAttribute {};

[__AttributeUsage(_AttributeTargets.Struct)]
public struct InputsAttribute {};

[__AttributeUsage(_AttributeTargets.Function)]
[__AttributeUsage(_AttributeTargets.Var)]
public struct ScaleAttribute { float scale; };

[__AttributeUsage(_AttributeTargets.Function)]
public struct FrameRateAttribute { int fps; };

[__AttributeUsage(_AttributeTargets.Function)]
public struct ReachAttribute { float keyWidths; };

[__AttributeUsage(_AttributeTargets.Function)]
public struct LingerAttribute { float seconds; };

[__AttributeUsage(_AttributeTargets.Function)]
public struct WritesAttribute { string target; };

[__AttributeUsage(_AttributeTargets.Var)]
public struct FormatAttribute { string format; };

[__AttributeUsage(_AttributeTargets.Var)]
public struct FeedbackAttribute {};

public struct Frame {
    public float time;
    public float delta;
    public uint index;
    public float2 resolution;
    public float2 panel;
};

public struct Pixel {
    public float2 uv;
    public float2 position;
};

public uniform Frame frame;

// Smooth bump: 1 at `centre`, 0 from `radius` out. Close to a gaussian, without exp.
public half bump(float2 p, float2 centre, float radius) {
    float2 d = p - centre;
    half x = half(saturate(1.0 - dot(d, d) / (radius * radius)));
    return x * x * (3.0h - 2.0h * x);
}

// Interleaved gradient noise added at 8-bit scale, which breaks up banding in slow gradients.
public half3 dither(half3 c, Pixel p) {
    float2 at = p.uv * frame.resolution;
    float noise = frac(52.9829189 * frac(dot(at, float2(0.06711056, 0.00583715))));
    return c + half((noise - 0.5) / 255.0);
}
