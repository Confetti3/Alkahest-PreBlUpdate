struct VSOutput {
    float4 position : SV_POSITION;
    float2 uv : TEXCOORD0;
};

static const float4 vertices[4] = {
    float4(-1.0, 1.0, 0.0, 1.0),
    float4(1.0, 1.0, 0.0, 1.0),
    float4(-1.0, -1.0, 0.0, 1.0),
    float4(1.0, -1.0, 0.0, 1.0)
};

static const float2 uvs[4] = {
    float2(0.0, 0.0),
    float2(1.0, 0.0),
    float2(0.0, 1.0),
    float2(1.0, 1.0)
};

VSOutput mainVS(uint vertex_id : SV_VertexID) {
    VSOutput output;
    output.position = vertices[vertex_id];
    output.uv = uvs[vertex_id];
    return output;
}

Texture2D SpecularIbl : register(t0);

float4 mainPS(VSOutput input) : SV_TARGET {
    return SpecularIbl.Load(int3(int2(input.position.xy), 0));
}
