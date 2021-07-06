#version 450

layout (vertices = 3) out;


layout (push_constant) uniform NodePC {
  mat4 view_transform;
  float node_width;
  float scale;
  vec2 viewport_dims;
  uint texture_period;
} node_uniform;



void main() {
  if (gl_InvocationID == 0)
    {
      gl_TessLevelInner[0] = 1.0;
      gl_TessLevelOuter[0] = 1.0;
      gl_TessLevelOuter[1] = 1.0;
      gl_TessLevelOuter[2] = 1.0;
    }

  gl_out[gl_InvocationID].gl_Position = gl_in[gl_InvocationID].gl_Position;
}