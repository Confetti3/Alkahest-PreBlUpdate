cbuffer ShadowkeepCubemapConstants : register(b11)
{
    float4x4 model_to_world;
    float4x4 world_to_model;
    float4x4 target_pixel_to_world;
    float4x4 world_to_projective;
    float4x4 world_to_cubemap;
    float4 camera_position;
    float4 intensity;
    float4 fade_params;
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

    float3 localPosition =
        mul(world_to_model, float4(world_position, 1.0)).xyz;

    float distanceToEdge =
        1.0 - max(
            max(abs(localPosition.x), abs(localPosition.y)),
            abs(localPosition.z)
        );

    float volumeWeight =
        pow(
            saturate(distanceToEdge),
            max(fade_params.x, 0.0001)
        );

    float3 reflected = reflect(-V, N);

    float3 cubemapDirection =
        normalize(
            mul((float3x3)world_to_cubemap, reflected)
        );

    uint width;
    uint height;
    uint mipCount;
    SpecularIbl.GetDimensions(0, width, height, mipCount);

    float maxMip =
        max((float)mipCount - 1.0, 0.0);

    float lod =
        sqrt(saturate(roughness)) * maxMip;

    float3 environment =
        SpecularIbl.SampleLevel(
            SamplerLinear,
            cubemapDirection,
            lod
        ).rgb;

    lighting_specular = float4(
        environment
            * max(intensity.x, 0.0)
            * volumeWeight,
        volumeWeight
    );

    lighting_diffuse = 0.0;
}
