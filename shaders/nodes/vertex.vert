#version 450

#define VERTICES_PER_NODE 2

layout (location = 0) in vec2 position;
layout (location = 0) out int node_id;

layout (push_constant) uniform View {
  float node_width;
  float scale;
  vec2 viewport_dims;
  mat4 view;
} vo;

void main() {
  gl_Position = vo.view * vec4(position, 0.0, 1.0);

  // NodeIds are 1-indexed
  int id = 1 + (gl_VertexIndex / VERTICES_PER_NODE);
  node_id = id;
}