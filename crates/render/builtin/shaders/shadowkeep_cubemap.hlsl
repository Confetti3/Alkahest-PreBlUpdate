cbuffer ShadowkeepCubemapConstants : register(b11)
{
    float4x4 model_to_world;
    float4x4 world_to_model;
    float4x4 target_pixel_to_world;
    float4x4 world_to_projective;
    float4 camera_position;
    float4 target_resolution;
};

struct VSOutput
{
    float4 position : SV_POSITION;
};

VSOutput mainVS(float3 in_position : POSITION)
{
    VSOutput output;
    output.position = mul(world_to_projective, mul(model_to_world, float4(in_position, 1.0)));
    return output;
}

TextureCube SpecularIbl : register(t0);
Texture3D DiffuseIbl : register(t1);
Texture2D RtNormal : register(t2);
Texture2D RtDepth : register(t3);
SamplerState SamplerLinear : register(s1);

void mainPS(
    VSOutput input,
    out float4 lighting_diffuse : SV_Target0,
    out float4 lighting_specular : SV_Target1)
{
    int2 pixel = int2(input.position.xy);
    float depth = RtDepth.Load(int3(pixel, 0)).x;
    float4 world_position_h = mul(target_pixel_to_world, float4(input.position.xy, depth, 1.0));
    float3 world_position = world_position_h.xyz / world_position_h.w;
    float4 packed_normal = RtNormal.Load(int3(pixel, 0));
    if (dot(abs(packed_normal.xyz), 1.0) < 0.0001)
    {
        discard;
    }
    float3 normal = packed_normal.xyz * 2.0 - 1.0;
    float smoothness = saturate(length(normal) * 4.0 - 3.0);
    float roughness = 1.0 - smoothness;
    float3 N = normalize(normal);
    float3 V = normalize(camera_position.xyz - world_position);
    float3 reflected = 2.0 * max(0.0, dot(N, V)) * N - V;

    uint width;
    uint height;
    uint mip_count;
    SpecularIbl.GetDimensions(0, width, height, mip_count);
    lighting_specular = float4(
        SpecularIbl.SampleLevel(SamplerLinear, reflected, sqrt(roughness) * mip_count).rgb,
        1.0
    );
    lighting_diffuse = 0.0;
}
