// egui renderer for D3D11.
//
// Mirrors egui-wgpu's "gamma framebuffer" path: vertex colors and textures are
// premultiplied, gamma-encoded (non-sRGB-aware) values; blending happens in gamma
// space into a UNORM backbuffer. Positions arrive in egui points.

cbuffer Locals : register(b0) {
    float2 screen_size_points;
    float2 _padding;
};

struct VsIn {
    float2 pos   : POSITION;
    float2 uv    : TEXCOORD0;
    float4 color : COLOR0;
};

struct VsOut {
    float4 pos   : SV_POSITION;
    float2 uv    : TEXCOORD0;
    float4 color : COLOR0;
};

VsOut vs_main(VsIn i) {
    VsOut o;
    o.pos = float4(
        2.0 * i.pos.x / screen_size_points.x - 1.0,
        1.0 - 2.0 * i.pos.y / screen_size_points.y,
        0.0,
        1.0);
    o.uv = i.uv;
    o.color = i.color;
    return o;
}

Texture2D tex : register(t0);
SamplerState samp : register(s0);

float4 ps_main(VsOut i) : SV_TARGET {
    return i.color * tex.Sample(samp, i.uv);
}
