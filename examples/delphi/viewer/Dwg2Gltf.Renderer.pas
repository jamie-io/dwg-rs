// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

{ Dwg2Gltf.Renderer - draws a dwg2gltf.dll scene with Direct3D 11.

  Upload copies the scene into GPU buffers once: per mesh one vertex buffer
  (position, normal, UV interleaved), one index buffer for its triangle and
  line submeshes and one for its visible edges (a line list into the same
  vertices); per texture a shader resource view with mipmaps; and one
  instance buffer with every instance's world and normal matrix, sorted by
  mesh. Render draws each submesh of a mesh once for all its instances
  (DrawIndexedInstanced): the number of draw calls follows the number of
  different meshes and materials, not the number of block instances.

  Shading: Blinn-Phong with a light at the camera, slightly above and left
  of it, plus a sky/ground ambient; base color times texture; transparent
  materials are drawn after the opaque ones, sorted back to front, without
  depth writes. Every triangle material of dwg2gltf.dll is double-sided:
  no culling, and the pixel shader turns the normal towards the viewer on
  back faces (SV_IsFrontFace). Single-sided materials are culled.

  Modes: shaded, shaded with edges, hidden line (white faces, black
  edges), wireframe (edges only), triangles (the tessellation as wire) and
  x-ray (faint faces, all edges). Edges are the DLL's visible edges; the
  faces under them are pushed back a little with the rasterizer's depth
  bias, so edges win the depth test. Lines are drawn as quads by a geometry
  shader, so their width can follow the screen's DPI.

  Silhouettes: the DLL's edges are creases and boundaries only, so a smooth
  curved surface has none along its outline. With Outlines on, shaded with
  edges and hidden line also write each pixel's normal and 1 / depth into
  a second target; a full-screen pass then draws a line where 1 / depth
  bends (it is linear across any plane, so only outlines and steps show)
  or the normal jumps (parts meeting at an angle).

  X-ray: weighted blended order-independent transparency. The faces add
  their premultiplied colors into one target and multiply a second one by
  (1 - alpha); a full-screen pass composes the average color with the
  total coverage. The result does not depend on the drawing order, so it
  stays still while the view turns.

  Coordinates: the renderer works in the scene's frame minus Origin (given
  to Upload, usually the center of the scene's bounds), computed in double
  from DwgNode.world: floats keep sub-millimeter precision even in drawings
  placed far from their origin. }
unit Dwg2Gltf.Renderer;

{$POINTERMATH ON}

interface

uses
  Winapi.Windows, Winapi.D3D11, Winapi.D3DCommon, Winapi.DxgiFormat, System.SysUtils,
  Dwg2Gltf, Dwg2Gltf.Math, Dwg2Gltf.Camera, Dwg2Gltf.D3D11;

type
  TRenderMode = (rmShaded, rmShadedEdges, rmHiddenLine, rmWireframe, rmTriangles, rmXRay);
  TMaterialPreset = (mpDrawing, mpWhiteClay, mpGreyMetal, mpGlass);
  TMarkerKind = (mkNone, mkSurface, mkVertex, mkMidpoint, mkEdge, mkCenter); // as TSnapKind
  TColor4 = TFourSingleArray; // linear RGBA

  TGpuMesh = record
    VB, IB, EB: ID3D11Buffer;
    EdgeIndexCount: UINT;
    Submeshes: TArray<TDwgSubmesh>;
    Center: TVec3; // of the mesh bounds, block coordinates
  end;

  TGpuMaterial = record
    Color: TColor4;
    Texture: Integer; // index into the SRVs, -1 for none
    DoubleSided, Blend, Unlit: Boolean;
    SpecStrength, Shininess: Single;
  end;

  TGpuInstance = record
    Mesh: Integer;
    Node: Integer;
    World: TMat4;      // double, relative to Origin
    Mirrored: Boolean; // flips the winding: cull the other side
    Slot: Integer;     // position in the instance buffer
  end;

  { Instances of one mesh with the same winding: consecutive slots of the
    instance buffer, drawn together. }
  TInstanceGroup = record
    Mesh: Integer;
    Mirrored: Boolean;
    First, Count: Integer;
  end;

  TOverlayLine = record
    A, B: TVec3;
    Color: TColor4;
  end;

  TOverlayMarker = record
    P: TVec3;
    Kind: TMarkerKind;
    Color: TColor4;
  end;

  TDwgRenderer = class
  private
    FDev: TD3DDevice;
    FVS, FFullVS: ID3D11VertexShader;
    FPS, FXRayPS, FOutlinePS, FCompositePS: ID3D11PixelShader;
    FLineGS: ID3D11GeometryShader;
    FLayout: ID3D11InputLayout;
    FFrameCB, FObjectCB: ID3D11Buffer;
    FOverlayVB: ID3D11Buffer;
    FInstanceVB: ID3D11Buffer;
    FSampler: ID3D11SamplerState;
    // rasterizer states: [depth bias for edges][cull: none, back, front]
    FRasterSolid: array[Boolean, 0..2] of ID3D11RasterizerState;
    FRasterWire: ID3D11RasterizerState;
    FDepthWrite, FDepthTest, FDepthOff: ID3D11DepthStencilState;
    FBlendAlpha, FBlendXRay: ID3D11BlendState;
    FMeshes: TArray<TGpuMesh>;
    FMaterials: TArray<TGpuMaterial>;
    FTextures: TArray<ID3D11ShaderResourceView>;
    FInstances: TArray<TGpuInstance>;
    FGroups: TArray<TInstanceGroup>;
    FHighlight: TArray<Boolean>;
    FUploadBytes: Int64;
    FDrawCalls: Integer;
    FWarnings: TArray<string>;
    FCurrentMesh: Integer;
    FCamera: TOrbitCamera;
    procedure CreatePipeline;
    function MakeBuffer(Bind: UINT; Data: Pointer; Bytes: NativeUInt): ID3D11Buffer;
    function UploadTexture(const Tex: TDwgTexture): ID3D11ShaderResourceView;
    procedure SetFrame(const Camera: TOrbitCamera);
    procedure SetObject(const Color: TColor4; Lit, Textured: Boolean; Spec, Shininess,
      Highlight: Single; LineWidth: Single = 1);
    procedure LineShader(On: Boolean);
    procedure FullScreenPass(PS: ID3D11PixelShader; const A, B: ID3D11ShaderResourceView);
    procedure UploadInstances;
    procedure BindMesh(Mesh: Integer);
    function EffectiveMaterial(Index: UINT): TGpuMaterial;
    procedure DrawFaces(Faces: TRenderMode; Transparent: Boolean; Bias: Boolean);
    procedure DrawLines(DepthTest: Boolean; Black: Boolean);
    procedure DrawEdges(DepthTest: Boolean; const Color: TColor4);
    procedure DrawOverlay;
  public
    Mode: TRenderMode;
    Preset: TMaterialPreset;
    Overlay: TArray<TOverlayLine>;
    Markers: TArray<TOverlayMarker>;
    { Silhouette lines in shaded with edges and hidden line. }
    Outlines: Boolean;
    { Screen pixels per 96-DPI pixel: line widths, outline and marker size. }
    PixelScale: Single;
    constructor Create(Dev: TD3DDevice);
    destructor Destroy; override;
    { Copy the scene to the GPU (the scene may be freed afterwards). }
    procedure Upload(const Data: TDwgSceneData; const Origin: TVec3);
    procedure Clear;
    procedure Render(Target: TRenderTarget; const Camera: TOrbitCamera);
    { Highlight the instances whose flag is set (nil: none). }
    procedure SetHighlight(const Flags: TArray<Boolean>);
    property Instances: TArray<TGpuInstance> read FInstances;
    property Groups: TArray<TInstanceGroup> read FGroups;
    property UploadBytes: Int64 read FUploadBytes;
    property DrawCalls: Integer read FDrawCalls;
    { Textures that could not be decoded and the like. }
    property Warnings: TArray<string> read FWarnings;
  end;

const
  RenderModeNames: array[TRenderMode] of string =
    ('Shaded', 'Shaded with edges', 'Hidden line', 'Wireframe', 'Triangles', 'X-ray');
  MaterialPresetNames: array[TMaterialPreset] of string =
    ('As in the drawing', 'White clay', 'Grey metal', 'Glass');

function Color4(R, G, B: Single; A: Single = 1): TColor4;

implementation

uses
  Winapi.D3DCompiler, System.Math, System.Generics.Collections, System.Generics.Defaults;

const
  // One shader pair for everything: lit faces, flat faces, lines, edges.
  // Matrices are column-major in the buffer (HLSL's default packing), as
  // TMat4 and dwg2gltf.dll store them: mul(M, v) with column vectors.
  ShaderSource =
    'cbuffer Frame : register(b0) {'#10 +
    '  float4x4 ViewProj;'#10 +
    '  float4 Eye;         // xyz: camera position (w = 1), or towards the camera (w = 0, orthographic)'#10 +
    '  float4 LightDir;    // xyz: towards the light'#10 +
    '  float4 Sky;         // ambient from above'#10 +
    '  float4 Ground;      // ambient from below'#10 +
    '  float4 HighlightColor;'#10 +
    '  float4 DepthOrigin; // xyz: depth is measured from here along Forward'#10 +
    '  float4 Forward;     // xyz: view direction'#10 +
    '  float4 Viewport;    // xy: size in pixels, z: line width in pixels, w: outline step in pixels'#10 +
    '  float4 Outline;     // rgb: outline color, w: threshold for the bend of 1 / depth'#10 +
    '};'#10 +
    'cbuffer Object : register(b1) {'#10 +
    '  float4 Color;       // linear RGBA'#10 +
    '  float4 Shade;       // x: specular, y: shininess, z: lit (1) or flat (0), w: textured'#10 +
    '  float4 Extra;       // x: how much a highlighted instance takes the highlight color, y: line width factor'#10 +
    '};'#10 +
    'Texture2D BaseColorTex : register(t0);'#10 +
    'SamplerState Smp : register(s0);'#10 +
    'Texture2D AuxA : register(t1); // outlines: normal and 1 / depth; x-ray: summed colors'#10 +
    'Texture2D AuxB : register(t2); // x-ray: what still shows through'#10 +
    'struct VSIn {'#10 +
    '  float3 pos : POSITION; float3 nrm : NORMAL; float2 uv : TEXCOORD0;'#10 +
    '  // per instance: the world matrix and the normal matrix by columns'#10 +
    '  float4 w0 : WORLD0; float4 w1 : WORLD1; float4 w2 : WORLD2; float4 w3 : WORLD3;'#10 +
    '  float3 n0 : NORMALMATRIX0; float3 n1 : NORMALMATRIX1; float3 n2 : NORMALMATRIX2;'#10 +
    '  float4 flags : INSTANCEFLAGS; // x: highlighted'#10 +
    '};'#10 +
    'struct PSIn {'#10 +
    '  float4 pos : SV_Position; float3 wpos : WORLDPOS; float3 nrm : NORMAL; float2 uv : TEXCOORD0;'#10 +
    '  nointerpolation float highlight : HIGHLIGHT;'#10 +
    '};'#10 +
    'PSIn VS(VSIn i) {'#10 +
    '  PSIn o;'#10 +
    '  float4 wp = i.w0 * i.pos.x + i.w1 * i.pos.y + i.w2 * i.pos.z + i.w3;'#10 +
    '  o.pos = mul(ViewProj, wp);'#10 +
    '  o.wpos = wp.xyz;'#10 +
    '  o.nrm = i.n0 * i.nrm.x + i.n1 * i.nrm.y + i.n2 * i.nrm.z;'#10 +
    '  o.uv = i.uv;'#10 +
    '  o.highlight = i.flags.x * Extra.x;'#10 +
    '  return o;'#10 +
    '}'#10 +
    '// a line as a quad of Viewport.z * Extra.y pixels, clipped at the near plane first'#10 +
    '[maxvertexcount(4)]'#10 +
    'void GSLine(line PSIn v[2], inout TriangleStream<PSIn> s) {'#10 +
    '  float4 p0 = v[0].pos, p1 = v[1].pos;'#10 +
    '  if (p0.z < 0 && p1.z < 0) return;'#10 +
    '  if (p0.z < 0) p0 = lerp(p0, p1, p0.z / (p0.z - p1.z));'#10 +
    '  if (p1.z < 0) p1 = lerp(p1, p0, p1.z / (p1.z - p0.z));'#10 +
    '  float2 a = p0.xy / p0.w * Viewport.xy, b = p1.xy / p1.w * Viewport.xy; // twice the pixels'#10 +
    '  float2 d = b - a;'#10 +
    '  d = dot(d, d) > 1e-12 ? normalize(d) : float2(1, 0);'#10 +
    '  float2 t = d * (Viewport.z * Extra.y / Viewport.xy); // half a width, in clip units'#10 +
    '  float2 n = float2(-d.y, d.x) * (Viewport.z * Extra.y / Viewport.xy);'#10 +
    '  PSIn o = v[0];'#10 +
    '  o.pos = float4(p0.xy + (-t + n) * p0.w, p0.zw); s.Append(o);'#10 +
    '  o.pos = float4(p0.xy + (-t - n) * p0.w, p0.zw); s.Append(o);'#10 +
    '  o = v[1];'#10 +
    '  o.pos = float4(p1.xy + (t + n) * p1.w, p1.zw); s.Append(o);'#10 +
    '  o.pos = float4(p1.xy + (t - n) * p1.w, p1.zw); s.Append(o);'#10 +
    '}'#10 +
    'float4 ShadeColor(PSIn i, float3 n, float3 v) {'#10 +
    '  float4 c = Color;'#10 +
    '  if (Shade.w > 0) c *= BaseColorTex.Sample(Smp, i.uv);'#10 +
    '  c.rgb = lerp(c.rgb, HighlightColor.rgb, i.highlight);'#10 +
    '  if (Shade.z == 0) return c;'#10 +
    '  float3 l = LightDir.xyz;'#10 +
    '  float diffuse = saturate(dot(n, l));'#10 +
    '  float3 h = normalize(l + v);'#10 +
    '  float spec = diffuse > 0 ? pow(saturate(dot(n, h)), Shade.y) * Shade.x : 0;'#10 +
    '  float3 ambient = lerp(Ground.rgb, Sky.rgb, n.y * 0.5 + 0.5);'#10 +
    '  return float4(c.rgb * (ambient + 0.68 * diffuse) + spec, c.a);'#10 +
    '}'#10 +
    'float3 FacingNormal(PSIn i, bool front, float3 v) {'#10 +
    '  float len = length(i.nrm);'#10 +
    '  float3 n = len > 1e-12 ? i.nrm / len : v;'#10 +
    '  return front ? n : -n; // double-sided: the side we see'#10 +
    '}'#10 +
    'float3 ToEye(PSIn i) { return Eye.w > 0 ? normalize(Eye.xyz - i.wpos) : Eye.xyz; }'#10 +
    'struct PSOut { float4 color : SV_Target0; float4 aux : SV_Target1; };'#10 +
    'PSOut PS(PSIn i, bool front : SV_IsFrontFace) {'#10 +
    '  float3 v = ToEye(i);'#10 +
    '  float3 n = FacingNormal(i, front, v);'#10 +
    '  PSOut o;'#10 +
    '  o.color = ShadeColor(i, n, v);'#10 +
    '  // for the outlines (when a second target is bound): the normal and'#10 +
    '  // 1 / depth, negative for highlighted instances (0 is the background)'#10 +
    '  float r = 1 / max(dot(i.wpos - DepthOrigin.xyz, Forward.xyz), 1e-9);'#10 +
    '  o.aux = float4(n, i.highlight > 0 ? -r : r);'#10 +
    '  return o;'#10 +
    '}'#10 +
    'struct XRayOut { float4 sum : SV_Target0; float4 reveal : SV_Target1; };'#10 +
    'XRayOut PSXRay(PSIn i, bool front : SV_IsFrontFace) {'#10 +
    '  float3 v = ToEye(i);'#10 +
    '  float4 c = ShadeColor(i, FacingNormal(i, front, v), v);'#10 +
    '  XRayOut o;'#10 +
    '  o.sum = float4(c.rgb * c.a, c.a); // added up'#10 +
    '  o.reveal = c.aaaa;                // multiplies the target by 1 - alpha'#10 +
    '  return o;'#10 +
    '}'#10 +
    '// a triangle that covers the screen'#10 +
    'float4 VSFull(uint id : SV_VertexID) : SV_Position {'#10 +
    '  float2 uv = float2((id << 1) & 2, id & 2);'#10 +
    '  return float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);'#10 +
    '}'#10 +
    'float4 Aux(int2 p) { return AuxA.Load(int3(clamp(p, int2(0, 0), int2(Viewport.xy) - 1), 0)); }'#10 +
    'float4 PSOutline(float4 pos : SV_Position) : SV_Target {'#10 +
    '  int2 p = int2(pos.xy);'#10 +
    '  int st = max(int(Viewport.w), 1);'#10 +
    '  float4 c = Aux(p);'#10 +
    '  float r = abs(c.w);'#10 +
    '  if (r == 0) discard; // background: the line goes on the object''s side'#10 +
    '  float4 l = Aux(p - int2(st, 0)), rt = Aux(p + int2(st, 0));'#10 +
    '  float4 u = Aux(p - int2(0, st)), d = Aux(p + int2(0, st));'#10 +
    '  // 1 / depth is linear across a plane: its second difference is zero'#10 +
    '  // there, and negative where this pixel lies in front of its neighbours'#10 +
    '  float bend = -min(abs(l.w) + abs(rt.w) - 2 * r, abs(u.w) + abs(d.w) - 2 * r) / r;'#10 +
    '  float e = saturate((bend - Outline.w) / Outline.w);'#10 +
    '  // the normal jumps: parts meeting at an angle (right and down only: one pixel wide)'#10 +
    '  if (abs(rt.w) > 0) e = max(e, saturate((0.75 - dot(c.xyz, rt.xyz)) * 4));'#10 +
    '  if (abs(d.w) > 0) e = max(e, saturate((0.75 - dot(c.xyz, d.xyz)) * 4));'#10 +
    '  if (e <= 0) discard;'#10 +
    '  return float4(c.w < 0 ? HighlightColor.rgb : Outline.rgb, e);'#10 +
    '}'#10 +
    'float4 PSComposite(float4 pos : SV_Position) : SV_Target {'#10 +
    '  int3 p = int3(pos.xy, 0);'#10 +
    '  float reveal = AuxB.Load(p).r;'#10 +
    '  if (reveal >= 1) discard;'#10 +
    '  float4 sum = AuxA.Load(p);'#10 +
    '  return float4(sum.rgb / max(sum.a, 1e-5), 1 - reveal);'#10 +
    '}'#10;

type
  TVertex = record
    Pos: array[0..2] of Single;
    Normal: array[0..2] of Single;
    UV: array[0..1] of Single;
  end;

  TFrameConstants = record
    ViewProj: TMat4F;
    Eye, LightDir, Sky, Ground, Highlight, DepthOrigin, Forward, Viewport, Outline: TColor4;
  end;

  TObjectConstants = record
    Color, Shade, Extra: TColor4;
    Pad: TColor4; // constant buffers are multiples of 16 bytes; 64 is tidy
  end;

  { One instance in the instance buffer (input slot 1). }
  TInstanceData = record
    World: TMat4F;                   // 4 columns
    Normal: array[0..2, 0..3] of Single; // 3 columns, xyz used
    Flags: TColor4;                  // x: highlighted
  end;

const
  OverlayCapacity = 1024; // vertices
  LineWidth = 1.2;        // edges, in 96-DPI pixels
  EdgeColor: TColor4 = (0.02, 0.02, 0.025, 1);
  HighlightColor: TColor4 = (1.0, 0.33, 0.02, 1);
  CullNone = 0;
  CullBack = 1;
  CullFront = 2;

function Color4(R, G, B: Single; A: Single): TColor4;
begin
  Result[0] := R;
  Result[1] := G;
  Result[2] := B;
  Result[3] := A;
end;

function Compile(const Entry, Target: AnsiString): ID3DBlob;
var
  Src: AnsiString;
  Errors: ID3DBlob;
  HR: HRESULT;
  Msg: string;
begin
  Src := ShaderSource;
  try
    HR := D3DCompile(PAnsiChar(Src), Length(Src), 'viewer.hlsl', nil, nil, PAnsiChar(Entry),
      PAnsiChar(Target), D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, Result, Errors);
  except
    on E: Exception do // delayed import: d3dcompiler_47.dll missing
      raise EGraphics.Create('d3dcompiler_47.dll: ' + E.Message);
  end;
  if Failed(HR) then
  begin
    Msg := '';
    if Errors <> nil then
      SetString(Msg, PAnsiChar(Errors.GetBufferPointer), Errors.GetBufferSize);
    raise EGraphics.CreateFmt('shader %s: %s', [string(Entry), Msg]);
  end;
end;

{ TDwgRenderer }

constructor TDwgRenderer.Create(Dev: TD3DDevice);
begin
  inherited Create;
  FDev := Dev;
  Mode := rmShadedEdges;
  Preset := mpDrawing;
  Outlines := True;
  PixelScale := 1;
  CreatePipeline;
end;

destructor TDwgRenderer.Destroy;
begin
  Clear;
  inherited;
end;

procedure TDwgRenderer.CreatePipeline;
var
  VSCode, PSCode, Code: ID3DBlob;
  Elements: array[0..10] of D3D11_INPUT_ELEMENT_DESC;
  I: Integer;
  Desc: D3D11_BUFFER_DESC;
  Raster: D3D11_RASTERIZER_DESC;
  Depth: D3D11_DEPTH_STENCIL_DESC;
  Blend: D3D11_BLEND_DESC;
  Sampler: D3D11_SAMPLER_DESC;
  Bias: Boolean;
  Cull: Integer;
begin
  VSCode := Compile('VS', 'vs_4_0');
  PSCode := Compile('PS', 'ps_4_0');
  Check(FDev.Device.CreateVertexShader(VSCode.GetBufferPointer, VSCode.GetBufferSize, nil,
    @FVS), 'CreateVertexShader');
  Check(FDev.Device.CreatePixelShader(PSCode.GetBufferPointer, PSCode.GetBufferSize, nil,
    FPS), 'CreatePixelShader');
  Code := Compile('PSXRay', 'ps_4_0');
  Check(FDev.Device.CreatePixelShader(Code.GetBufferPointer, Code.GetBufferSize, nil,
    FXRayPS), 'CreatePixelShader');
  Code := Compile('PSOutline', 'ps_4_0');
  Check(FDev.Device.CreatePixelShader(Code.GetBufferPointer, Code.GetBufferSize, nil,
    FOutlinePS), 'CreatePixelShader');
  Code := Compile('PSComposite', 'ps_4_0');
  Check(FDev.Device.CreatePixelShader(Code.GetBufferPointer, Code.GetBufferSize, nil,
    FCompositePS), 'CreatePixelShader');
  Code := Compile('VSFull', 'vs_4_0');
  Check(FDev.Device.CreateVertexShader(Code.GetBufferPointer, Code.GetBufferSize, nil,
    @FFullVS), 'CreateVertexShader');
  Code := Compile('GSLine', 'gs_4_0');
  Check(FDev.Device.CreateGeometryShader(Code.GetBufferPointer, Code.GetBufferSize, nil,
    FLineGS), 'CreateGeometryShader');

  FillChar(Elements, SizeOf(Elements), 0);
  Elements[0].SemanticName := 'POSITION';
  Elements[0].Format := DXGI_FORMAT_R32G32B32_FLOAT;
  Elements[0].AlignedByteOffset := 0;
  Elements[1].SemanticName := 'NORMAL';
  Elements[1].Format := DXGI_FORMAT_R32G32B32_FLOAT;
  Elements[1].AlignedByteOffset := 12;
  Elements[2].SemanticName := 'TEXCOORD';
  Elements[2].Format := DXGI_FORMAT_R32G32_FLOAT;
  Elements[2].AlignedByteOffset := 24;
  // per instance, from the instance buffer in slot 1
  for I := 3 to 10 do
  begin
    Elements[I].InputSlot := 1;
    Elements[I].InputSlotClass := D3D11_INPUT_PER_INSTANCE_DATA;
    Elements[I].InstanceDataStepRate := 1;
    Elements[I].AlignedByteOffset := (I - 3) * 16;
    Elements[I].Format := DXGI_FORMAT_R32G32B32A32_FLOAT;
    case I of
      3..6:
        begin
          Elements[I].SemanticName := 'WORLD';
          Elements[I].SemanticIndex := I - 3;
        end;
      7..9:
        begin
          Elements[I].SemanticName := 'NORMALMATRIX';
          Elements[I].SemanticIndex := I - 7;
          Elements[I].Format := DXGI_FORMAT_R32G32B32_FLOAT;
        end;
      10:
        Elements[I].SemanticName := 'INSTANCEFLAGS';
    end;
  end;
  Check(FDev.Device.CreateInputLayout(@Elements[0], Length(Elements), VSCode.GetBufferPointer,
    VSCode.GetBufferSize, FLayout), 'CreateInputLayout');

  FillChar(Desc, SizeOf(Desc), 0);
  Desc.Usage := D3D11_USAGE_DYNAMIC;
  Desc.BindFlags := D3D11_BIND_CONSTANT_BUFFER;
  Desc.CPUAccessFlags := D3D11_CPU_ACCESS_WRITE;
  Desc.ByteWidth := SizeOf(TFrameConstants);
  Check(FDev.Device.CreateBuffer(Desc, nil, FFrameCB), 'CreateBuffer(frame)');
  Desc.ByteWidth := SizeOf(TObjectConstants);
  Check(FDev.Device.CreateBuffer(Desc, nil, FObjectCB), 'CreateBuffer(object)');
  Desc.BindFlags := D3D11_BIND_VERTEX_BUFFER;
  Desc.ByteWidth := OverlayCapacity * SizeOf(TVertex);
  Check(FDev.Device.CreateBuffer(Desc, nil, FOverlayVB), 'CreateBuffer(overlay)');

  FillChar(Raster, SizeOf(Raster), 0);
  Raster.FillMode := D3D11_FILL_SOLID;
  Raster.FrontCounterClockwise := True; // glTF: counter-clockwise is the front
  Raster.DepthClipEnable := True;
  Raster.MultisampleEnable := FDev.Samples > 1;
  for Bias := False to True do
    for Cull := CullNone to CullFront do
    begin
      case Cull of
        CullNone: Raster.CullMode := D3D11_CULL_NONE;
        CullBack: Raster.CullMode := D3D11_CULL_BACK;
        CullFront: Raster.CullMode := D3D11_CULL_FRONT;
      end;
      // faces under edges: about 2 steps of the 24-bit depth buffer, more
      // where the face is steep to the view
      if Bias then
      begin
        Raster.DepthBias := 2;
        Raster.SlopeScaledDepthBias := 1.5;
      end
      else
      begin
        Raster.DepthBias := 0;
        Raster.SlopeScaledDepthBias := 0;
      end;
      Check(FDev.Device.CreateRasterizerState(Raster, FRasterSolid[Bias, Cull]),
        'CreateRasterizerState');
    end;
  Raster.FillMode := D3D11_FILL_WIREFRAME;
  Raster.CullMode := D3D11_CULL_NONE;
  Raster.DepthBias := 0;
  Raster.SlopeScaledDepthBias := 0;
  Check(FDev.Device.CreateRasterizerState(Raster, FRasterWire), 'CreateRasterizerState');

  FillChar(Depth, SizeOf(Depth), 0);
  Depth.DepthEnable := True;
  Depth.DepthWriteMask := D3D11_DEPTH_WRITE_MASK_ALL;
  Depth.DepthFunc := D3D11_COMPARISON_LESS_EQUAL;
  Depth.FrontFace.StencilFailOp := D3D11_STENCIL_OP_KEEP;
  Depth.FrontFace.StencilDepthFailOp := D3D11_STENCIL_OP_KEEP;
  Depth.FrontFace.StencilPassOp := D3D11_STENCIL_OP_KEEP;
  Depth.FrontFace.StencilFunc := D3D11_COMPARISON_ALWAYS;
  Depth.BackFace := Depth.FrontFace;
  Check(FDev.Device.CreateDepthStencilState(Depth, FDepthWrite), 'CreateDepthStencilState');
  Depth.DepthWriteMask := D3D11_DEPTH_WRITE_MASK_ZERO;
  Check(FDev.Device.CreateDepthStencilState(Depth, FDepthTest), 'CreateDepthStencilState');
  Depth.DepthEnable := False;
  Check(FDev.Device.CreateDepthStencilState(Depth, FDepthOff), 'CreateDepthStencilState');

  FillChar(Blend, SizeOf(Blend), 0);
  Blend.RenderTarget[0].BlendEnable := True;
  Blend.RenderTarget[0].SrcBlend := D3D11_BLEND_SRC_ALPHA;
  Blend.RenderTarget[0].DestBlend := D3D11_BLEND_INV_SRC_ALPHA;
  Blend.RenderTarget[0].BlendOp := D3D11_BLEND_OP_ADD;
  Blend.RenderTarget[0].SrcBlendAlpha := D3D11_BLEND_ONE;
  Blend.RenderTarget[0].DestBlendAlpha := D3D11_BLEND_INV_SRC_ALPHA;
  Blend.RenderTarget[0].BlendOpAlpha := D3D11_BLEND_OP_ADD;
  Blend.RenderTarget[0].RenderTargetWriteMask := $0F;
  Check(FDev.Device.CreateBlendState(Blend, FBlendAlpha), 'CreateBlendState');
  // x-ray: add colors into target 0, multiply target 1 by 1 - alpha
  Blend.IndependentBlendEnable := True;
  Blend.RenderTarget[0].SrcBlend := D3D11_BLEND_ONE;
  Blend.RenderTarget[0].DestBlend := D3D11_BLEND_ONE;
  Blend.RenderTarget[0].SrcBlendAlpha := D3D11_BLEND_ONE;
  Blend.RenderTarget[0].DestBlendAlpha := D3D11_BLEND_ONE;
  Blend.RenderTarget[1] := Blend.RenderTarget[0];
  Blend.RenderTarget[1].SrcBlend := D3D11_BLEND_ZERO;
  Blend.RenderTarget[1].DestBlend := D3D11_BLEND_INV_SRC_COLOR;
  Blend.RenderTarget[1].SrcBlendAlpha := D3D11_BLEND_ZERO;
  Blend.RenderTarget[1].DestBlendAlpha := D3D11_BLEND_INV_SRC_ALPHA;
  Check(FDev.Device.CreateBlendState(Blend, FBlendXRay), 'CreateBlendState');

  FillChar(Sampler, SizeOf(Sampler), 0);
  Sampler.Filter := D3D11_FILTER_MIN_MAG_MIP_LINEAR;
  Sampler.AddressU := D3D11_TEXTURE_ADDRESS_WRAP; // dwg2gltf.dll: always repeat
  Sampler.AddressV := D3D11_TEXTURE_ADDRESS_WRAP;
  Sampler.AddressW := D3D11_TEXTURE_ADDRESS_WRAP;
  Sampler.ComparisonFunc := D3D11_COMPARISON_NEVER;
  Sampler.MaxLOD := D3D11_FLOAT32_MAX;
  Check(FDev.Device.CreateSamplerState(Sampler, FSampler), 'CreateSamplerState');
end;

function TDwgRenderer.MakeBuffer(Bind: UINT; Data: Pointer; Bytes: NativeUInt): ID3D11Buffer;
var
  Desc: D3D11_BUFFER_DESC;
  Init: D3D11_SUBRESOURCE_DATA;
begin
  FillChar(Desc, SizeOf(Desc), 0);
  Desc.ByteWidth := Bytes;
  Desc.Usage := D3D11_USAGE_IMMUTABLE;
  Desc.BindFlags := Bind;
  FillChar(Init, SizeOf(Init), 0);
  Init.pSysMem := Data;
  Check(FDev.Device.CreateBuffer(Desc, @Init, Result), 'CreateBuffer');
  Inc(FUploadBytes, Bytes);
end;

function TDwgRenderer.UploadTexture(const Tex: TDwgTexture): ID3D11ShaderResourceView;
var
  Pixels: TBytes;
  W, H: Integer;
  Desc: D3D11_TEXTURE2D_DESC;
  Texture: ID3D11Texture2D;
begin
  Pixels := DecodeImage(Tex.data, Tex.size, W, H);
  // a full mip chain, filled by the GPU (GenerateMips needs a render target)
  FillChar(Desc, SizeOf(Desc), 0);
  Desc.Width := W;
  Desc.Height := H;
  Desc.MipLevels := 0;
  Desc.ArraySize := 1;
  Desc.Format := DXGI_FORMAT_R8G8B8A8_UNORM_SRGB; // base color images are sRGB
  Desc.SampleDesc.Count := 1;
  Desc.Usage := D3D11_USAGE_DEFAULT;
  Desc.BindFlags := D3D11_BIND_SHADER_RESOURCE or D3D11_BIND_RENDER_TARGET;
  Desc.MiscFlags := Ord(D3D11_RESOURCE_MISC_GENERATE_MIPS);
  Check(FDev.Device.CreateTexture2D(Desc, nil, Texture), 'CreateTexture2D(texture)');
  FDev.Context.UpdateSubresource(Texture, 0, nil, PByte(Pixels), W * 4, 0);
  Check(FDev.Device.CreateShaderResourceView(Texture, nil, Result), 'CreateShaderResourceView');
  FDev.Context.GenerateMips(Result);
  Inc(FUploadBytes, Int64(W) * H * 4 * 4 div 3);
end;

procedure TDwgRenderer.Clear;
begin
  FMeshes := nil;
  FMaterials := nil;
  FTextures := nil;
  FInstances := nil;
  FGroups := nil;
  FInstanceVB := nil;
  FHighlight := nil;
  FWarnings := nil;
  Overlay := nil;
  Markers := nil;
  FUploadBytes := 0;
end;

procedure TDwgRenderer.Upload(const Data: TDwgSceneData; const Origin: TVec3);
var
  I, J: Integer;
  Mesh: PDwgMesh;
  Mat: PDwgMaterial;
  Vertices: TArray<TVertex>;
  Node: PDwgNode;
  World: TMat4;
  Rough: Single;
  Order: TArray<Integer>;
begin
  Clear;
  SetLength(FTextures, Data.texture_count);
  for I := 0 to High(FTextures) do
    try
      FTextures[I] := UploadTexture(Data.textures[I]);
    except
      on E: Exception do
        FWarnings := FWarnings + [Format('texture %d (%s): %s',
          [I, DwgStr(Data.textures[I].name), E.Message])];
    end;

  SetLength(FMaterials, Data.material_count);
  for I := 0 to High(FMaterials) do
  begin
    Mat := @Data.materials[I];
    Move(Mat.base_color, FMaterials[I].Color, SizeOf(TColor4));
    FMaterials[I].Texture := -1;
    if (Mat.texture >= 0) and (Mat.texture < Length(FTextures)) and
      (FTextures[Mat.texture] <> nil) then
      FMaterials[I].Texture := Mat.texture;
    FMaterials[I].DoubleSided := Mat.flags and DWG_MAT_DOUBLE_SIDED <> 0;
    FMaterials[I].Blend := (Mat.flags and DWG_MAT_BLEND <> 0) or (Mat.base_color[3] < 1);
    FMaterials[I].Unlit := Mat.flags and DWG_MAT_UNLIT <> 0;
    // a rough guess from the PBR values: shiny and smooth gives a small,
    // bright highlight, rough a faint wide one
    Rough := EnsureRange(Mat.roughness, 0.05, 1);
    FMaterials[I].SpecStrength := (0.04 + 0.5 * Mat.metallic) * (1 - Rough) + 0.03;
    FMaterials[I].Shininess := EnsureRange(2 / (Rough * Rough * Rough * Rough) - 2, 4, 256);
  end;

  SetLength(FMeshes, Data.mesh_count);
  for I := 0 to High(FMeshes) do
  begin
    Mesh := @Data.meshes[I];
    FMeshes[I].Center := TVec3.Create(
      (Mesh.bounds_min[0] + Mesh.bounds_max[0]) / 2,
      (Mesh.bounds_min[1] + Mesh.bounds_max[1]) / 2,
      (Mesh.bounds_min[2] + Mesh.bounds_max[2]) / 2);
    SetLength(FMeshes[I].Submeshes, Mesh.submesh_count);
    for J := 0 to Integer(Mesh.submesh_count) - 1 do
      FMeshes[I].Submeshes[J] := Mesh.submeshes[J];
    if Mesh.vertex_count = 0 then
      Continue;
    SetLength(Vertices, Mesh.vertex_count);
    for J := 0 to Integer(Mesh.vertex_count) - 1 do
    begin
      Move(Mesh.positions[3 * J], Vertices[J].Pos, 12);
      Move(Mesh.normals[3 * J], Vertices[J].Normal, 12);
      if Mesh.uvs <> nil then
        Move(Mesh.uvs[2 * J], Vertices[J].UV, 8)
      else
      begin
        Vertices[J].UV[0] := 0;
        Vertices[J].UV[1] := 0;
      end;
    end;
    FMeshes[I].VB := MakeBuffer(D3D11_BIND_VERTEX_BUFFER, @Vertices[0],
      Length(Vertices) * SizeOf(TVertex));
    if Mesh.index_count > 0 then
      FMeshes[I].IB := MakeBuffer(D3D11_BIND_INDEX_BUFFER, Mesh.indices,
        Mesh.index_count * SizeOf(UInt32));
    if Mesh.edge_count > 0 then
    begin
      FMeshes[I].EB := MakeBuffer(D3D11_BIND_INDEX_BUFFER, Mesh.edges,
        Mesh.edge_count * 2 * SizeOf(UInt32));
      FMeshes[I].EdgeIndexCount := Mesh.edge_count * 2;
    end;
  end;

  SetLength(FInstances, Data.instance_count);
  for I := 0 to High(FInstances) do
  begin
    FInstances[I].Mesh := Data.instances[I].mesh;
    FInstances[I].Node := Data.instances[I].node;
    // the node's double matrix, moved by -Origin before going to float
    Node := @Data.nodes[Data.instances[I].node];
    World := TMat4.FromDoubles(Node.world);
    World.M[12] := World.M[12] - Origin.X;
    World.M[13] := World.M[13] - Origin.Y;
    World.M[14] := World.M[14] - Origin.Z;
    FInstances[I].World := World;
    FInstances[I].Mirrored := World.Det3 < 0;
  end;
  SetLength(FHighlight, Length(FInstances));

  // slots sorted by mesh and winding; groups of equal ones
  SetLength(Order, Length(FInstances));
  for I := 0 to High(Order) do
    Order[I] := I;
  TArray.Sort<Integer>(Order, TComparer<Integer>.Construct(
    function(const A, B: Integer): Integer
    begin
      Result := FInstances[A].Mesh - FInstances[B].Mesh;
      if Result = 0 then
        Result := Ord(FInstances[A].Mirrored) - Ord(FInstances[B].Mirrored);
      if Result = 0 then
        Result := A - B;
    end));
  for I := 0 to High(Order) do
  begin
    FInstances[Order[I]].Slot := I;
    if (FGroups = nil) or (FGroups[High(FGroups)].Mesh <> FInstances[Order[I]].Mesh) or
      (FGroups[High(FGroups)].Mirrored <> FInstances[Order[I]].Mirrored) then
    begin
      SetLength(FGroups, Length(FGroups) + 1);
      FGroups[High(FGroups)].Mesh := FInstances[Order[I]].Mesh;
      FGroups[High(FGroups)].Mirrored := FInstances[Order[I]].Mirrored;
      FGroups[High(FGroups)].First := I;
      FGroups[High(FGroups)].Count := 0;
    end;
    Inc(FGroups[High(FGroups)].Count);
  end;
  UploadInstances;
end;

procedure TDwgRenderer.UploadInstances;
var
  Data: TArray<TInstanceData>;
  Desc: D3D11_BUFFER_DESC;
  Init: D3D11_SUBRESOURCE_DATA;
  I, R, C: Integer;
  N: TMat4;
begin
  // one more slot at the end: the identity, for the overlay
  SetLength(Data, Length(FInstances) + 1);
  for I := 0 to High(FInstances) do
    with Data[FInstances[I].Slot] do
    begin
      World := ToFloat(FInstances[I].World);
      N := FInstances[I].World.NormalMatrix;
      for C := 0 to 2 do
      begin
        for R := 0 to 2 do
          Normal[C, R] := N.Get(R, C);
        Normal[C, 3] := 0;
      end;
      Flags := Color4(Ord(FHighlight[I]), 0, 0, 0);
    end;
  with Data[High(Data)] do
  begin
    World := ToFloat(TMat4.Identity);
    FillChar(Normal, SizeOf(Normal), 0);
    Normal[0, 0] := 1;
    Normal[1, 1] := 1;
    Normal[2, 2] := 1;
    Flags := Color4(0, 0, 0, 0);
  end;
  if FInstanceVB = nil then
  begin
    FillChar(Desc, SizeOf(Desc), 0);
    Desc.ByteWidth := Length(Data) * SizeOf(TInstanceData);
    Desc.Usage := D3D11_USAGE_DEFAULT; // rewritten when the highlight changes
    Desc.BindFlags := D3D11_BIND_VERTEX_BUFFER;
    FillChar(Init, SizeOf(Init), 0);
    Init.pSysMem := @Data[0];
    Check(FDev.Device.CreateBuffer(Desc, @Init, FInstanceVB), 'CreateBuffer(instances)');
    Inc(FUploadBytes, Desc.ByteWidth);
  end
  else
    FDev.Context.UpdateSubresource(FInstanceVB, 0, nil, @Data[0], 0, 0);
end;

procedure TDwgRenderer.SetHighlight(const Flags: TArray<Boolean>);
var
  I: Integer;
begin
  for I := 0 to High(FHighlight) do
    FHighlight[I] := (I < Length(Flags)) and Flags[I];
  if FInstanceVB <> nil then
    UploadInstances;
end;

function TDwgRenderer.EffectiveMaterial(Index: UINT): TGpuMaterial;
begin
  if Integer(Index) < Length(FMaterials) then
    Result := FMaterials[Index]
  else
  begin
    Result.Color := Color4(0.8, 0.8, 0.8);
    Result.Texture := -1;
    Result.DoubleSided := True;
    Result.Blend := False;
    Result.Unlit := False;
    Result.SpecStrength := 0.1;
    Result.Shininess := 32;
  end;
  if Preset = mpDrawing then
    Exit;
  // presets replace the look of faces; lines keep their colors
  Result.Texture := -1;
  Result.Blend := False;
  case Preset of
    mpWhiteClay:
      begin
        Result.Color := Color4(0.80, 0.79, 0.76);
        Result.SpecStrength := 0.04;
        Result.Shininess := 8;
      end;
    mpGreyMetal:
      begin
        Result.Color := Color4(0.42, 0.44, 0.47);
        Result.SpecStrength := 0.7;
        Result.Shininess := 48;
      end;
    mpGlass:
      begin
        Result.Color := Color4(0.55, 0.72, 0.80, 0.3);
        Result.Blend := True;
        Result.SpecStrength := 0.9;
        Result.Shininess := 96;
      end;
  end;
end;

procedure TDwgRenderer.SetFrame(const Camera: TOrbitCamera);
var
  C: TFrameConstants;
  Mapped: D3D11_MAPPED_SUBRESOURCE;
  F, R, U, L, P: TVec3;
begin
  Camera.Basis(F, R, U);
  C.ViewProj := ToFloat(Camera.ViewProj);
  if Camera.Ortho then
    C.Eye := Color4(-F.X, -F.Y, -F.Z, 0)
  else
    C.Eye := Color4(Camera.Eye.X, Camera.Eye.Y, Camera.Eye.Z, 1);
  // headlight, a little above and left of the eye so faces facing the
  // camera are not all equally bright
  L := (-F + U * 0.45 - R * 0.3).Normalized;
  C.LightDir := Color4(L.X, L.Y, L.Z, 0);
  C.Sky := Color4(0.30, 0.31, 0.34);
  C.Ground := Color4(0.12, 0.11, 0.10);
  C.Highlight := HighlightColor;
  // depth for the outlines: from the eye, or in orthographic views from a
  // point far enough behind it that everything has a positive depth
  if Camera.Ortho then
    P := Camera.Target - F * (Camera.Distance + 4 * Max(Camera.SceneBox.Radius, Camera.Distance))
  else
    P := Camera.Eye;
  C.DepthOrigin := Color4(P.X, P.Y, P.Z, 1);
  C.Forward := Color4(F.X, F.Y, F.Z, 0);
  C.Viewport := Color4(Camera.Width, Camera.Height, LineWidth * PixelScale,
    Max(1, Round(PixelScale)));
  C.Outline := Color4(EdgeColor[0], EdgeColor[1], EdgeColor[2], 0.02);
  Check(FDev.Context.Map(FFrameCB, 0, D3D11_MAP_WRITE_DISCARD, 0, Mapped), 'Map');
  Move(C, Mapped.pData^, SizeOf(C));
  FDev.Context.Unmap(FFrameCB, 0);
end;

procedure TDwgRenderer.SetObject(const Color: TColor4; Lit, Textured: Boolean; Spec,
  Shininess, Highlight, LineWidth: Single);
var
  C: TObjectConstants;
  Mapped: D3D11_MAPPED_SUBRESOURCE;
begin
  C.Color := Color;
  C.Shade := Color4(Spec, Shininess, Ord(Lit), Ord(Textured));
  C.Extra := Color4(Highlight, LineWidth, 0, 0);
  C.Pad := C.Extra;
  Check(FDev.Context.Map(FObjectCB, 0, D3D11_MAP_WRITE_DISCARD, 0, Mapped), 'Map');
  Move(C, Mapped.pData^, SizeOf(C));
  FDev.Context.Unmap(FObjectCB, 0);
  Inc(FDrawCalls);
end;

procedure TDwgRenderer.BindMesh(Mesh: Integer);
var
  Stride, Offset: UINT;
begin
  if Mesh = FCurrentMesh then
    Exit;
  FCurrentMesh := Mesh;
  Stride := SizeOf(TVertex);
  Offset := 0;
  FDev.Context.IASetVertexBuffers(0, 1, FMeshes[Mesh].VB, @Stride, @Offset);
  FDev.Context.IASetIndexBuffer(FMeshes[Mesh].IB, DXGI_FORMAT_R32_UINT, 0);
end;

procedure TDwgRenderer.LineShader(On: Boolean);
var
  NoInstances: ID3D11ClassInstance;
begin
  if On then
  begin
    FDev.Context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_LINELIST);
    FDev.Context.GSSetShader(FLineGS, NoInstances, 0);
    FDev.Context.GSSetConstantBuffers(0, 1, FFrameCB);
    FDev.Context.GSSetConstantBuffers(1, 1, FObjectCB);
  end
  else
  begin
    FDev.Context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    FDev.Context.GSSetShader(nil, NoInstances, 0);
  end;
end;

procedure TDwgRenderer.FullScreenPass(PS: ID3D11PixelShader; const A,
  B: ID3D11ShaderResourceView);
var
  NoInstances: ID3D11ClassInstance;
  NoView: ID3D11ShaderResourceView;
begin
  with FDev.Context do
  begin
    IASetInputLayout(nil);
    IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    VSSetShader(FFullVS, NoInstances, 0);
    PSSetShader(PS, NoInstances, 0);
    PSSetShaderResources(1, 1, A);
    PSSetShaderResources(2, 1, B);
    RSSetState(FRasterSolid[False, CullNone]);
    OMSetDepthStencilState(FDepthOff, 0);
    Draw(3, 0);
    Inc(FDrawCalls);
    // unbind: the textures are render targets again next frame
    PSSetShaderResources(1, 1, NoView);
    PSSetShaderResources(2, 1, NoView);
    IASetInputLayout(FLayout);
    VSSetShader(FVS, NoInstances, 0);
    PSSetShader(FPS, NoInstances, 0);
  end;
  FCurrentMesh := -1;
end;

procedure TDwgRenderer.DrawFaces(Faces: TRenderMode; Transparent: Boolean; Bias: Boolean);
type
  TDrawItem = record
    Instance, Submesh: Integer;
    Depth: Double;
  end;
var
  Items: TList<TDrawItem>;
  Item: TDrawItem;
  G, I, J: Integer;
  F, R, U: TVec3;

  // one submesh: for Count instances from slot First
  procedure Draw(const Sub: TDwgSubmesh; Mirrored: Boolean; First, Count: Integer);
  var
    Mat: TGpuMaterial;
    Color: TColor4;
    Lit, Textured: Boolean;
    Cull: Integer;
  begin
    Mat := EffectiveMaterial(Sub.material);
    Lit := not Mat.Unlit;
    Textured := Mat.Texture >= 0;
    Color := Mat.Color;
    case Faces of
      rmHiddenLine:
        begin
          Color := Color4(1, 1, 1);
          Lit := False;
          Textured := False;
        end;
      rmXRay:
        begin
          Color[3] := 0.12;
          Textured := False;
        end;
      rmTriangles:
        begin
          Lit := False;
          Textured := False;
          Color[3] := 1;
        end;
    end;
    // single-sided: cull the back, which a mirroring instance turns into
    // the front
    Cull := CullNone;
    if not Mat.DoubleSided and (Faces in [rmShaded, rmShadedEdges]) then
      if Mirrored then
        Cull := CullFront
      else
        Cull := CullBack;
    if Faces = rmTriangles then
      FDev.Context.RSSetState(FRasterWire)
    else
      FDev.Context.RSSetState(FRasterSolid[Bias, Cull]);
    if Textured then
      FDev.Context.PSSetShaderResources(0, 1, FTextures[Mat.Texture]);
    SetObject(Color, Lit, Textured, Mat.SpecStrength, Mat.Shininess, 0.55);
    FDev.Context.DrawIndexedInstanced(Sub.index_count, Count, Sub.first_index, 0, First);
  end;

  function Wanted(const Sub: TDwgSubmesh): Boolean;
  begin
    Result := Sub.topology = DWG_TRIANGLES;
    // hidden line and x-ray ignore the materials' transparency
    if Result and (Faces in [rmShaded, rmShadedEdges]) then
      Result := EffectiveMaterial(Sub.material).Blend = Transparent;
  end;

begin
  if not Transparent then
  begin
    // all instances of a mesh at once
    for G := 0 to High(FGroups) do
      with FGroups[G] do
      begin
        if FMeshes[Mesh].VB = nil then
          Continue;
        BindMesh(Mesh);
        for J := 0 to High(FMeshes[Mesh].Submeshes) do
          if Wanted(FMeshes[Mesh].Submeshes[J]) then
            Draw(FMeshes[Mesh].Submeshes[J], Mirrored, First, Count);
      end;
    Exit;
  end;

  // transparent: one instance at a time, back to front
  FCamera.Basis(F, R, U);
  Items := TList<TDrawItem>.Create;
  try
    for I := 0 to High(FInstances) do
    begin
      if FMeshes[FInstances[I].Mesh].VB = nil then
        Continue;
      for J := 0 to High(FMeshes[FInstances[I].Mesh].Submeshes) do
        if Wanted(FMeshes[FInstances[I].Mesh].Submeshes[J]) then
        begin
          Item.Instance := I;
          Item.Submesh := J;
          Item.Depth := FInstances[I].World.TransformPoint(
            FMeshes[FInstances[I].Mesh].Center).Dot(F);
          Items.Add(Item);
        end;
    end;
    Items.Sort(TComparer<TDrawItem>.Construct(
      function(const A, B: TDrawItem): Integer
      begin
        Result := CompareValue(B.Depth, A.Depth);
      end));
    for Item in Items do
      with FInstances[Item.Instance] do
      begin
        BindMesh(Mesh);
        Draw(FMeshes[Mesh].Submeshes[Item.Submesh], Mirrored, Slot, 1);
      end;
  finally
    Items.Free;
  end;
end;

procedure TDwgRenderer.DrawLines(DepthTest: Boolean; Black: Boolean);
var
  G, J: Integer;
  Sub: TDwgSubmesh;
  Color: TColor4;
begin
  // wire geometry of the drawing (lines, arcs, polylines): flat colored
  FDev.Context.RSSetState(FRasterSolid[False, CullNone]);
  if DepthTest then
    FDev.Context.OMSetDepthStencilState(FDepthWrite, 0)
  else
    FDev.Context.OMSetDepthStencilState(FDepthOff, 0);
  for G := 0 to High(FGroups) do
    with FGroups[G] do
    begin
      if FMeshes[Mesh].VB = nil then
        Continue;
      for J := 0 to High(FMeshes[Mesh].Submeshes) do
      begin
        Sub := FMeshes[Mesh].Submeshes[J];
        if Sub.topology <> DWG_LINES then
          Continue;
        if not Black and (Integer(Sub.material) < Length(FMaterials)) then
          Color := FMaterials[Sub.material].Color
        else
          Color := EdgeColor;
        Color[3] := 1;
        BindMesh(Mesh);
        SetObject(Color, False, False, 0, 1, 1);
        FDev.Context.DrawIndexedInstanced(Sub.index_count, Count, Sub.first_index, 0, First);
      end;
    end;
end;

procedure TDwgRenderer.DrawEdges(DepthTest: Boolean; const Color: TColor4);
var
  G: Integer;
begin
  FDev.Context.RSSetState(FRasterSolid[False, CullNone]);
  if DepthTest then
    FDev.Context.OMSetDepthStencilState(FDepthTest, 0)
  else
    FDev.Context.OMSetDepthStencilState(FDepthOff, 0);
  SetObject(Color, False, False, 0, 1, 1);
  for G := 0 to High(FGroups) do
    with FGroups[G] do
    begin
      if FMeshes[Mesh].EB = nil then
        Continue;
      BindMesh(Mesh);
      FDev.Context.IASetIndexBuffer(FMeshes[Mesh].EB, DXGI_FORMAT_R32_UINT, 0);
      FDev.Context.DrawIndexedInstanced(FMeshes[Mesh].EdgeIndexCount, Count, 0, 0, First);
      Inc(FDrawCalls);
    end;
  FCurrentMesh := -1; // the triangle index buffer is no longer bound
end;

procedure TDwgRenderer.DrawOverlay;
var
  Verts: TArray<TVertex>;
  Colors: TArray<TColor4>;
  Mapped: D3D11_MAPPED_SUBRESOURCE;
  F, R, U: TVec3;
  I, J, N: Integer;
  S: Double;
  Stride, Offset: UINT;
  BlendFactor: TFourSingleArray;

  procedure AddLine(const A, B: TVec3; const C: TColor4);
  begin
    if Length(Verts) + 2 > OverlayCapacity then
      Exit;
    SetLength(Verts, Length(Verts) + 2);
    FillChar(Verts[High(Verts) - 1], 2 * SizeOf(TVertex), 0);
    Verts[High(Verts) - 1].Pos[0] := A.X;
    Verts[High(Verts) - 1].Pos[1] := A.Y;
    Verts[High(Verts) - 1].Pos[2] := A.Z;
    Verts[High(Verts)].Pos[0] := B.X;
    Verts[High(Verts)].Pos[1] := B.Y;
    Verts[High(Verts)].Pos[2] := B.Z;
    Colors := Colors + [C];
  end;

  procedure Add(const A, B: TVec3; const C: TColor4);
  begin
    AddLine(A, B, C);
  end;

begin
  if (Overlay = nil) and (Markers = nil) then
    Exit;
  FCamera.Basis(F, R, U);
  for I := 0 to High(Overlay) do
    Add(Overlay[I].A, Overlay[I].B, Overlay[I].Color);
  // markers: a few pixels across whatever the zoom, in the screen plane
  for I := 0 to High(Markers) do
  begin
    S := 6 * PixelScale * FCamera.PixelSize(Markers[I].P);
    with Markers[I] do
      case Kind of
        mkVertex: // square
          begin
            Add(P + (-R - U) * S, P + (R - U) * S, Color);
            Add(P + (R - U) * S, P + (R + U) * S, Color);
            Add(P + (R + U) * S, P + (-R + U) * S, Color);
            Add(P + (-R + U) * S, P + (-R - U) * S, Color);
          end;
        mkMidpoint: // triangle
          begin
            Add(P + (-R - U * 0.6) * S, P + (R - U * 0.6) * S, Color);
            Add(P + (R - U * 0.6) * S, P + U * S, Color);
            Add(P + U * S, P + (-R - U * 0.6) * S, Color);
          end;
        mkEdge: // hourglass
          begin
            Add(P + (-R - U) * S, P + (R + U) * S, Color);
            Add(P + (R - U) * S, P + (-R + U) * S, Color);
            Add(P + (-R + U) * S, P + (R + U) * S, Color);
            Add(P + (-R - U) * S, P + (R - U) * S, Color);
          end;
        mkCenter: // octagon
          for J := 0 to 7 do
            Add(P + (R * Cos(J * Pi / 4) + U * Sin(J * Pi / 4)) * S,
              P + (R * Cos((J + 1) * Pi / 4) + U * Sin((J + 1) * Pi / 4)) * S, Color);
        mkSurface: // cross
          begin
            Add(P - R * S, P + R * S, Color);
            Add(P - U * S, P + U * S, Color);
          end;
      end;
  end;
  N := Length(Verts);
  if N = 0 then
    Exit;
  Check(FDev.Context.Map(FOverlayVB, 0, D3D11_MAP_WRITE_DISCARD, 0, Mapped), 'Map');
  Move(Verts[0], Mapped.pData^, N * SizeOf(TVertex));
  FDev.Context.Unmap(FOverlayVB, 0);
  FCurrentMesh := -1;
  Stride := SizeOf(TVertex);
  Offset := 0;
  FDev.Context.IASetVertexBuffers(0, 1, FOverlayVB, @Stride, @Offset);
  LineShader(True);
  FDev.Context.RSSetState(FRasterSolid[False, CullNone]);
  FDev.Context.OMSetDepthStencilState(FDepthOff, 0);
  FillChar(BlendFactor, SizeOf(BlendFactor), 0);
  FDev.Context.OMSetBlendState(nil, BlendFactor, $FFFFFFFF);
  // the identity instance after the scene's instances
  for I := 0 to N div 2 - 1 do
  begin
    SetObject(Colors[I], False, False, 0, 1, 0, 2); // two line widths
    FDev.Context.DrawInstanced(2, 1, 2 * I, Length(FInstances));
  end;
  LineShader(False);
end;

procedure TDwgRenderer.Render(Target: TRenderTarget; const Camera: TOrbitCamera);
var
  Background: TColor4;
  BlendFactor: TFourSingleArray;
  NoInstances: ID3D11ClassInstance;
  Edges, Bias: Boolean;
  Stride, Offset: UINT;
  Targets: array[0..1] of ID3D11RenderTargetView;
  Zero, One: TFourSingleArray;
begin
  Zero := Color4(0, 0, 0, 0);
  One := Color4(1, 1, 1, 1);
  FDrawCalls := 0;
  FCurrentMesh := -1;
  FCamera := Camera;
  if Mode = rmHiddenLine then
    Background := Color4(1, 1, 1)
  else
    Background := Color4(0.40, 0.42, 0.46);
  Target.BeginFrame(Background);
  SetFrame(Camera);

  with FDev.Context do
  begin
    IASetInputLayout(FLayout);
    VSSetShader(FVS, NoInstances, 0);
    PSSetShader(FPS, NoInstances, 0);
    VSSetConstantBuffers(0, 1, FFrameCB);
    VSSetConstantBuffers(1, 1, FObjectCB);
    PSSetConstantBuffers(0, 1, FFrameCB);
    PSSetConstantBuffers(1, 1, FObjectCB);
    PSSetSamplers(0, 1, FSampler);
    FillChar(BlendFactor, SizeOf(BlendFactor), 0);
    OMSetBlendState(nil, BlendFactor, $FFFFFFFF);
  end;
  if FInstanceVB = nil then
    Exit; // nothing uploaded
  Stride := SizeOf(TInstanceData);
  Offset := 0;
  FDev.Context.IASetVertexBuffers(1, 1, FInstanceVB, @Stride, @Offset);

  Edges := Mode in [rmShadedEdges, rmHiddenLine, rmWireframe, rmXRay];
  Bias := Mode in [rmShadedEdges, rmHiddenLine];
  LineShader(False);
  case Mode of
    rmShaded, rmShadedEdges, rmHiddenLine:
      begin
        FDev.Context.OMSetDepthStencilState(FDepthWrite, 0);
        if Outlines and (Mode in [rmShadedEdges, rmHiddenLine]) then
        begin
          // opaque faces into color and normal + 1 / depth, then the outlines
          Targets[0] := Target.RTV;
          Targets[1] := Target.Aux(0, DXGI_FORMAT_R16G16B16A16_FLOAT);
          FDev.Context.ClearRenderTargetView(Targets[1], Zero);
          FDev.Context.OMSetRenderTargets(2, Targets[0], Target.DSV);
          DrawFaces(Mode, False, Bias);
          FDev.Context.OMSetRenderTargets(1, Target.RTV, Target.DSV);
          FDev.Context.OMSetBlendState(FBlendAlpha, BlendFactor, $FFFFFFFF);
          FullScreenPass(FOutlinePS, Target.ResolveAux(0), nil);
          FDev.Context.OMSetBlendState(nil, BlendFactor, $FFFFFFFF);
          FDev.Context.OMSetDepthStencilState(FDepthWrite, 0);
        end
        else
          DrawFaces(Mode, False, Bias);
        LineShader(True);
        DrawLines(True, Mode = rmHiddenLine);
        LineShader(False);
        if Mode <> rmHiddenLine then
        begin
          FDev.Context.OMSetDepthStencilState(FDepthTest, 0);
          FDev.Context.OMSetBlendState(FBlendAlpha, BlendFactor, $FFFFFFFF);
          DrawFaces(Mode, True, Bias);
          FDev.Context.OMSetBlendState(nil, BlendFactor, $FFFFFFFF);
        end;
      end;
    rmWireframe:
      begin
        LineShader(True);
        DrawLines(False, False);
      end;
    rmTriangles:
      begin
        FDev.Context.OMSetDepthStencilState(FDepthOff, 0);
        DrawFaces(Mode, False, False);
        LineShader(True);
        DrawLines(False, False);
      end;
    rmXRay:
      begin
        // order-independent: sums into two extra targets, then composed
        Targets[0] := Target.Aux(0, DXGI_FORMAT_R16G16B16A16_FLOAT);
        Targets[1] := Target.Aux(1, DXGI_FORMAT_R16_FLOAT);
        FDev.Context.ClearRenderTargetView(Targets[0], Zero);
        FDev.Context.ClearRenderTargetView(Targets[1], One);
        FDev.Context.OMSetRenderTargets(2, Targets[0], nil);
        FDev.Context.OMSetDepthStencilState(FDepthOff, 0);
        FDev.Context.OMSetBlendState(FBlendXRay, BlendFactor, $FFFFFFFF);
        FDev.Context.PSSetShader(FXRayPS, NoInstances, 0);
        DrawFaces(Mode, False, False);
        FDev.Context.PSSetShader(FPS, NoInstances, 0);
        FDev.Context.OMSetRenderTargets(1, Target.RTV, Target.DSV);
        FDev.Context.OMSetBlendState(FBlendAlpha, BlendFactor, $FFFFFFFF);
        FullScreenPass(FCompositePS, Target.ResolveAux(0), Target.ResolveAux(1));
        FDev.Context.OMSetBlendState(nil, BlendFactor, $FFFFFFFF);
        LineShader(True);
        DrawLines(False, False);
      end;
  end;
  if Edges then
  begin
    LineShader(True);
    DrawEdges(Mode in [rmShadedEdges, rmHiddenLine], EdgeColor);
  end;
  LineShader(False);
  DrawOverlay;
end;

end.
