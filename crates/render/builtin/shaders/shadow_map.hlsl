static const uint SAMPLE_NUM = 9;
static const float2 POISSON_SAMPLES[SAMPLE_NUM] =
{
    float2( 0.0f, 0.0f ),
    float2( -0.45570817558838717f, -0.8078111571344746f ),
    float2( 0.8960089702138108f, -0.3569415776898711f ),
    float2( -0.19807511750350795f, 0.9210419317460738f ),
    float2( 0.2595217206107728f, -0.8090512416351279f ),
    float2( 0.5972218724881266f, 0.7789149385216658f ),
    float2( -0.8088925785649114f, 0.45309126356924434f ),
    float2( -0.9167903846266768f, -0.21303266118076764f ),
    float2( 0.6772830402709191f, 0.18318818607984763f ),
};

cbuffer scope_view : register(b12)
{
    float4x4 world_to_projective : packoffset(c0);
    float4x4 camera_to_world : packoffset(c4);

    float4 target : packoffset(c8);
    float4 view_miscellaneous : packoffset(c9);
    float4 view_unk20 : packoffset(c10);
    // float4x4 camera_to_projective : packoffset(c11);
}; // cbuffer scope_view

#define camera_position (transpose(camera_to_world)[3].xyz)
#define camera_backward (transpose(camera_to_world)[2].xyz)
#define camera_up (transpose(camera_to_world)[1].xyz)
#define camera_right (transpose(camera_to_world)[0].xyz)
#define camera_forward (-transpose(camera_to_world)[2].xyz)
#define camera_down (-transpose(camera_to_world)[1].xyz)
#define camera_left (-transpose(camera_to_world)[0].xyz)
#define target_width (target.x)
#define target_height (target.y)
#define target_resolution (target.xy)
#define inverse_target_resolution (target.zw)
#define maximum_depth_pre_projection (view_miscellaneous.x)
#define view_is_first_person (view_miscellaneous.y)

// Vertex Shader
struct VSOutput
{
    float4 pos : SV_POSITION;
    float2 uv : TEXCOORD0;
    float3 normal : NORMAL;
};

static const float4 vertices[4] = {
    float4(-1.0, 1.0, 0.0, 1.0),  // Top Left
    float4(1.0, 1.0, 0.0, 1.0),   // Top Right
    float4(-1.0, -1.0, 0.0, 1.0), // Bottom Left
    float4(1.0, -1.0, 0.0, 1.0)   // Bottom Right
};

static const float2 uvs[4] = {
    float2(0.0, 0.0), // Top Left
    float2(1.0, 0.0), // Top Right
    float2(0.0, 1.0), // Bottom Left
    float2(1.0, 1.0)  // Bottom Right
};


cbuffer scope_shadowmap : register(b0)
{
    float4x4 target_pixel_to_world;
    float4x4 camera_to_projective;
    float4x4 world_to_camera;
    float4x4 cascade_world_to_viewport;
    float4 light_dir;
    float plane_distance;
};

VSOutput mainVS(uint vertexID: SV_VertexID)
{
    VSOutput output;

    float3 multiply = float3 (0, 0, 0);
    multiply.x = 1.0f / camera_to_projective[0][0];
    multiply.y = 1.0f / camera_to_projective[1][1];

    output.pos = vertices[vertexID];
    output.uv = uvs[vertexID];

    float3 tempPos = (output.pos.xyz * multiply) - float3(0, 0, 1);
    output.normal = mul(transpose((float3x3)world_to_camera), normalize(tempPos));

    return output;
}

// Pixel Shader
Texture2D deferred_depth : register(t0);
Texture2D cascade_shadowmap : register(t1);
SamplerState samplerState : register(s1);

float2 GetTexelSize(Texture2D tex) {
    uint width, height;
    tex.GetDimensions(width, height);
    return float2(1.0f / width, 1.0f / height);
}

float InterleavedGradientNoise(float2 position_screen)
{
    float3 magic = float3(0.06711056f, 0.00583715f, 52.9829189f);
    return frac(magic.z * frac(dot(position_screen, magic.xy)));
}

float4 mainPS(VSOutput input) : SV_TARGET
{
    // The shadow mask is half resolution, so retain UV-based depth sampling.
    float depth = deferred_depth.SampleLevel(samplerState, input.uv, 0).x;

    // Reverse-Z clear value: no scene geometry.
    if (depth <= 0.000001)
        return float4(1.0, 1.0, 1.0, 1.0);

    float4 worldPos =
        mul(target_pixel_to_world, float4(input.pos.xy, depth, 1.0));

    if (abs(worldPos.w) <= 0.000001)
        discard;

    worldPos /= worldPos.w;

    // Select the nearest cascade that covers this main-camera depth.
    float viewDistance = -mul(world_to_camera, worldPos).z;
    if (viewDistance < 0.0 || viewDistance > plane_distance)
        discard;

    float4 lightClip =
        mul(cascade_world_to_viewport, float4(worldPos.xyz, 1.0));

    if (abs(lightClip.w) <= 0.000001)
        discard;

    float3 lightNdc = lightClip.xyz / lightClip.w;

    float2 cascadeUv = float2(
        lightNdc.x * 0.5 + 0.5,
        1.0 - (lightNdc.y * 0.5 + 0.5)
    );

    if (any(cascadeUv < 0.0) || any(cascadeUv > 1.0))
        discard;

    // Critical: do not overwrite another cascade with white.
    if (lightNdc.z < 0.0 || lightNdc.z > 1.0)
        discard;

    float bias = 0.0001;
    float2 texelSize = GetTexelSize(cascade_shadowmap);
    float filterSpread = 2.5;

    float randomAngle =
        InterleavedGradientNoise(input.pos.xy) * 6.28318530718;

    float s = sin(randomAngle);
    float c = cos(randomAngle);
    float2x2 rotation = float2x2(c, -s, s, c);

    float shadow = 0.0;

    [unroll]
    for (int i = 0; i < SAMPLE_NUM; ++i)
    {
        float2 offset =
            mul(rotation, POISSON_SAMPLES[i])
            * texelSize
            * filterSpread;

        float shadowDepth =
            cascade_shadowmap.SampleLevel(
                samplerState,
                cascadeUv + offset,
                0
            ).x;

        shadow +=
            (lightNdc.z - bias) > shadowDepth
            ? 0.0
            : 1.0;
    }

    shadow /= float(SAMPLE_NUM);
    return float4(shadow.xxx, 1.0);
}
