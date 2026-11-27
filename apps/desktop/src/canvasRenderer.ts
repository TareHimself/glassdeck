import type { InputUpdate } from "./bindings";

/**
 * Draws a canvas the way glassdeck-gpu's `CanvasProgram` does on the keyboard: the same GLSL ES 3.00
 * shader, a full-screen triangle, the `frame` uniforms, and each input as the script last wrote it.
 * Only what changed is uploaded; the frame arrives from the app's side, which runs `keyboard.luau`.
 */

const VERTEX = `#version 300 es
void main() {
    vec2 corner = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(corner * 2.0 - 1.0, 0.0, 1.0);
}
`;

/** The glue's uniform holding the inputs, as `glassdeck_slang::canvas::INPUTS_UNIFORM` names it. */
const INPUTS_UNIFORM = "gd_inputs";
const PANEL = [1920, 1080] as const;

export type CanvasFrame = { time: number; delta: number; index: number; inputs: InputUpdate[] };

/** The member Slang's std140 wrapper struct keeps an array in. */
const STD140_ARRAY_MEMBER = ".data_0";

/**
 * `gd_inputs_0.roseAt_0` to `gd_inputs.roseAt`, and an array, `gd_inputs_0.bands_0[0]` or as Slang wraps
 * it for std140, `gd_inputs_0.bands_0.data_0[0]`, to `gd_inputs.bands`.
 */
export function slangName(glsl: string): string {
  const array = glsl.endsWith("[0]") ? glsl.slice(0, -"[0]".length) : null;
  const unwrapped = array?.endsWith(STD140_ARRAY_MEMBER) ? array.slice(0, -STD140_ARRAY_MEMBER.length) : array;
  return (unwrapped ?? glsl)
    .split(".")
    .map((part) => part.replace(/_\d+$/, ""))
    .join(".");
}

export class CanvasRenderer {
  private readonly gl: WebGL2RenderingContext;
  private program: WebGLProgram | null = null;
  private uniforms = new Map<string, WebGLUniformLocation>();
  private pending: CanvasFrame[] = [];
  private scheduled = 0;

  constructor(private readonly canvas: HTMLCanvasElement) {
    // preserveDrawingBuffer keeps the last frame readable, for tests and for a paused canvas
    const gl = canvas.getContext("webgl2", { antialias: false, premultipliedAlpha: false, preserveDrawingBuffer: true });
    if (!gl) throw new Error("this webview has no WebGL2");
    this.gl = gl;
    gl.bindVertexArray(gl.createVertexArray());
    // The keyboard scans fragment row 0 out at the top, but a canvas shows it at the bottom.
    canvas.style.transform = "scaleY(-1)";
  }

  /** A new shader for a new run; inputs start unset until its first frame writes them. */
  useProgram(fragment: string, width: number, height: number) {
    const gl = this.gl;
    const program = gl.createProgram();
    for (const [kind, source] of [
      [gl.VERTEX_SHADER, VERTEX],
      [gl.FRAGMENT_SHADER, fragment],
    ] as const) {
      const shader = gl.createShader(kind);
      if (!shader) throw new Error("creating a shader");
      gl.shaderSource(shader, source);
      gl.compileShader(shader);
      if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) throw new Error(`compiling the canvas: ${gl.getShaderInfoLog(shader)}`);
      gl.attachShader(program, shader);
      gl.deleteShader(shader);
    }
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) throw new Error(`linking the canvas: ${gl.getProgramInfoLog(program)}`);
    if (this.program) gl.deleteProgram(this.program);
    this.program = program;
    this.uniforms = new Map();
    const count = gl.getProgramParameter(program, gl.ACTIVE_UNIFORMS) as number;
    for (let i = 0; i < count; i++) {
      const active = gl.getActiveUniform(program, i);
      const location = active && gl.getUniformLocation(program, active.name);
      if (active && location) this.uniforms.set(slangName(active.name), location);
    }
    this.canvas.width = width;
    this.canvas.height = height;
    this.pending = [];
  }

  /** Queues a frame; frames that arrive between two animation frames are drawn as one. */
  frame(frame: CanvasFrame) {
    this.pending.push(frame);
    this.scheduled ||= requestAnimationFrame(() => this.draw());
  }

  clear() {
    this.pending = [];
    this.gl.clearColor(0, 0, 0, 0);
    this.gl.clear(this.gl.COLOR_BUFFER_BIT);
  }

  dispose() {
    cancelAnimationFrame(this.scheduled);
    if (this.program) this.gl.deleteProgram(this.program);
    this.gl.getExtension("WEBGL_lose_context")?.loseContext();
  }

  private draw() {
    this.scheduled = 0;
    const frames = this.pending;
    this.pending = [];
    const latest = frames.at(-1);
    if (!this.program || !latest) return;
    const gl = this.gl;
    const uniform = (name: string) => this.uniforms.get(name) ?? null;
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.useProgram(this.program);
    gl.uniform1f(uniform("frame.time"), latest.time);
    gl.uniform1f(uniform("frame.delta"), latest.delta);
    gl.uniform1ui(uniform("frame.index"), latest.index);
    gl.uniform2f(uniform("frame.resolution"), this.canvas.width, this.canvas.height);
    gl.uniform2f(uniform("frame.panel"), PANEL[0], PANEL[1]);
    for (const { name, value } of frames.flatMap((f) => f.inputs)) {
      const location = uniform(`${INPUTS_UNIFORM}.${name}`);
      switch (value.kind) {
        case "float": {
          const [x = 0, y = 0, z = 0, w = 0] = value.values.map((v) => v ?? 0);
          if (value.values.length === 1) gl.uniform1f(location, x);
          else if (value.values.length === 2) gl.uniform2f(location, x, y);
          else if (value.values.length === 3) gl.uniform3f(location, x, y, z);
          else gl.uniform4f(location, x, y, z, w);
          break;
        }
        case "uint":
          gl.uniform1ui(location, value.value);
          break;
        case "array":
          gl.uniform1fv(location, value.values.map((v) => v ?? 0));
          break;
      }
    }
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }
}
