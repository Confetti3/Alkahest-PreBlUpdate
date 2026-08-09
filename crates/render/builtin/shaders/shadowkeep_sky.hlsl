cbuffer ShadowkeepSkyConstants : register(b11)
{
    float4x4 target_pixel_to_world;
    float4 camera_position;
    float4 sun_direction;
    float4 zenith_color;
    float4 horizon_color;
    float4 sun_color;
};

struct VSOutput {
    float4 position : SV_POSITION;
};

static const float4 vertices[4] = {
    float4(-1.0, 1.0, 0.0, 1.0),
    float4(1.0, 1.0, 0.0, 1.0),
    float4(-1.0, -1.0, 0.0, 1.0),
    float4(1.0, -1.0, 0.0, 1.0)
};

VSOutput mainVS(uint vertex_id : SV_VertexID) {
    VSOutput output;
    output.position = vertices[vertex_id];
    return output;
}

Texture2D SceneDepth : register(t0);

float4 mainPS(VSOutput input) : SV_TARGET {
    float depth = SceneDepth.Load(int3(int2(input.position.xy), 0)).x;
    if (depth > 0.000001)
        discard;

    float4 world_h = mul(target_pixel_to_world, float4(input.position.xy, 0.0, 1.0));
    float3 view_direction = normalize(world_h.xyz / world_h.w - camera_position.xyz);
    float horizon = saturate(view_direction.z * 0.5 + 0.5);
    float3 sky = lerp(horizon_color.rgb, zenith_color.rgb, smoothstep(0.0, 1.0, horizon));

    float sun_alignment = saturate(dot(view_direction, normalize(sun_direction.xyz)));
    float glow = pow(sun_alignment, 64.0);
    float disk = smoothstep(0.99975, 0.99995, sun_alignment);
    sky += sun_color.rgb * (0.15 * glow + 8.0 * disk);
    return float4(sky, 1.0);
}
