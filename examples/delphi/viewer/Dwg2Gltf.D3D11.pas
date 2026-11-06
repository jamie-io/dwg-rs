// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

{ Dwg2Gltf.D3D11 - the Direct3D 11 plumbing of the viewer: the device
  (hardware, else WARP, Windows' software rasterizer, which is what a remote
  desktop session without a GPU gets), a render target with 4x MSAA that
  either presents into a window or is read back into memory, and image
  files through WIC (Windows Imaging Component).

  Only Windows' own DLLs are used: d3d11, dxgi, d3dcompiler_47 (for the
  shaders, see Dwg2Gltf.Renderer) and windowscodecs.

  Colors: the render target is R8G8B8A8_UNORM_SRGB, so the shaders work in
  linear RGB (as dwg2gltf.dll delivers its colors) and the hardware encodes
  sRGB on write. }
unit Dwg2Gltf.D3D11;

interface

uses
  Winapi.Windows, Winapi.D3D11, Winapi.D3DCommon, Winapi.DXGI, Winapi.DxgiFormat,
  Winapi.DxgiType, System.SysUtils, System.Math;

type
  EGraphics = class(Exception);

  { Device and immediate context. }
  TD3DDevice = class
  private
    FDevice: ID3D11Device;
    FContext: ID3D11DeviceContext;
    FFeatureLevel: D3D_FEATURE_LEVEL;
    FIsWarp: Boolean;
    FSamples: Integer;
  public
    { Hardware first, WARP when that fails (or right away with ForceWarp). }
    constructor Create(ForceWarp: Boolean);
    destructor Destroy; override;
    function Description: string;
    property Device: ID3D11Device read FDevice;
    property Context: ID3D11DeviceContext read FContext;
    property FeatureLevel: D3D_FEATURE_LEVEL read FFeatureLevel;
    property IsWarp: Boolean read FIsWarp;
    { MSAA sample count used for render targets: 4, or 1 without support. }
    property Samples: Integer read FSamples;
  end;

  { Color and depth buffers of one size. Render into RTV/DSV, then Present
    (window) or ReadPixels (offscreen). }
  TRenderTarget = class
  private
    FDev: TD3DDevice;
    FWindow: HWND;
    FSwapChain: IDXGISwapChain;
    FColor: ID3D11Texture2D;    // MSAA when Samples > 1
    FResolved: ID3D11Texture2D; // single sample, sRGB
    FDepth: ID3D11Texture2D;
    FRTV: ID3D11RenderTargetView;
    FDSV: ID3D11DepthStencilView;
    FWidth, FHeight: Integer;
    procedure CreateBuffers;
    procedure ReleaseBuffers;
    procedure Resolve;
  public
    { Window = 0 makes an offscreen target. }
    constructor Create(Dev: TD3DDevice; Window: HWND; Width, Height: Integer);
    destructor Destroy; override;
    procedure Resize(Width, Height: Integer);
    { Bind the target, set the viewport and clear color and depth. }
    procedure BeginFrame(const ClearColor: TFourSingleArray);
    { Show the frame in the window. False when the device was lost (driver
      update, remote session change): recreate device and target. }
    function Present: Boolean;
    { The frame as RGBA bytes, rows top down, sRGB encoded, alpha 255. }
    function ReadPixels: TBytes;
    property RTV: ID3D11RenderTargetView read FRTV;
    property DSV: ID3D11DepthStencilView read FDSV;
    property Width: Integer read FWidth;
    property Height: Integer read FHeight;
  end;

procedure Check(HR: HRESULT; const What: string);

{ Decode an image file in memory (PNG, JPEG, BMP, TIFF, GIF ...) into RGBA
  bytes, rows top down. }
function DecodeImage(Data: Pointer; Size: NativeUInt; out Width, Height: Integer): TBytes;
{ Write RGBA bytes as a PNG file. }
procedure SavePng(const FileName: string; const Pixels: TBytes; Width, Height: Integer);

implementation

uses
  Winapi.ActiveX, Winapi.Wincodec;

const
  ColorFormat = DXGI_FORMAT_R8G8B8A8_UNORM_SRGB;
  DepthFormat = DXGI_FORMAT_D24_UNORM_S8_UINT;

procedure Check(HR: HRESULT; const What: string);
begin
  if Failed(HR) then
    raise EGraphics.CreateFmt('%s failed (HRESULT %.8x)', [What, Cardinal(HR)]);
end;

{ TD3DDevice }

constructor TD3DDevice.Create(ForceWarp: Boolean);
const
  Levels: array[0..2] of D3D_FEATURE_LEVEL =
    (D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0);
var
  HR: HRESULT;
  Quality: UINT;

  function TryCreate(Driver: D3D_DRIVER_TYPE): HRESULT;
  begin
    // d3d11.dll is loaded on first use (delayed import): a missing
    // runtime surfaces here as an exception, not at program start
    try
      Result := D3D11CreateDevice(nil, Driver, 0, 0, @Levels[0], Length(Levels),
        D3D11_SDK_VERSION, FDevice, FFeatureLevel, FContext);
    except
      on E: Exception do
        raise EGraphics.Create('Direct3D 11 is not available: ' + E.Message);
    end;
  end;

begin
  inherited Create;
  HR := E_FAIL;
  if not ForceWarp then
    HR := TryCreate(D3D_DRIVER_TYPE_HARDWARE);
  if Failed(HR) then
  begin
    HR := TryCreate(D3D_DRIVER_TYPE_WARP);
    FIsWarp := Succeeded(HR);
  end;
  Check(HR, 'D3D11CreateDevice');
  // 4x MSAA is required at feature level 10.1+ for this format, but ask:
  // 10.0 hardware may not have it
  FSamples := 1;
  if Succeeded(FDevice.CheckMultisampleQualityLevels(ColorFormat, 4, Quality)) and
    (Quality > 0) and
    Succeeded(FDevice.CheckMultisampleQualityLevels(DepthFormat, 4, Quality)) and
    (Quality > 0) then
    FSamples := 4;
end;

destructor TD3DDevice.Destroy;
begin
  if FContext <> nil then
    FContext.ClearState;
  FContext := nil;
  FDevice := nil;
  inherited;
end;

function TD3DDevice.Description: string;
begin
  if FIsWarp then
    Result := 'WARP'
  else
    Result := 'hardware';
  Result := Format('Direct3D 11 %s, feature level %d.%d, %dx MSAA',
    [Result, FFeatureLevel shr 12, (FFeatureLevel shr 8) and $F, FSamples]);
end;

{ TRenderTarget }

constructor TRenderTarget.Create(Dev: TD3DDevice; Window: HWND; Width, Height: Integer);
var
  Desc: DXGI_SWAP_CHAIN_DESC;
  DxgiDevice: IDXGIDevice;
  Adapter: IDXGIAdapter;
  Factory: IDXGIFactory;
begin
  inherited Create;
  FDev := Dev;
  FWindow := Window;
  FWidth := Max(Width, 1);
  FHeight := Max(Height, 1);
  if Window <> 0 then
  begin
    // the factory that made the device makes its swap chain
    Check(Dev.Device.QueryInterface(IDXGIDevice, DxgiDevice), 'IDXGIDevice');
    Check(DxgiDevice.GetAdapter(Adapter), 'GetAdapter');
    Check(Adapter.GetParent(IDXGIFactory, Factory), 'GetParent(IDXGIFactory)');
    // a single-buffered blit swap chain without MSAA: works from Windows 7
    // on and over remote desktop; the frame is resolved and copied into it
    FillChar(Desc, SizeOf(Desc), 0);
    Desc.BufferDesc.Width := FWidth;
    Desc.BufferDesc.Height := FHeight;
    Desc.BufferDesc.Format := DXGI_FORMAT_R8G8B8A8_UNORM;
    Desc.SampleDesc.Count := 1;
    Desc.BufferUsage := DXGI_USAGE_RENDER_TARGET_OUTPUT;
    Desc.BufferCount := 1;
    Desc.OutputWindow := Window;
    Desc.Windowed := True;
    Desc.SwapEffect := DXGI_SWAP_EFFECT_DISCARD;
    Check(Factory.CreateSwapChain(Dev.Device, Desc, FSwapChain), 'CreateSwapChain');
    Factory.MakeWindowAssociation(Window, DXGI_MWA_NO_ALT_ENTER);
  end;
  CreateBuffers;
end;

destructor TRenderTarget.Destroy;
begin
  ReleaseBuffers;
  FSwapChain := nil;
  inherited;
end;

procedure TRenderTarget.CreateBuffers;
var
  Desc: D3D11_TEXTURE2D_DESC;
begin
  FillChar(Desc, SizeOf(Desc), 0);
  Desc.Width := FWidth;
  Desc.Height := FHeight;
  Desc.MipLevels := 1;
  Desc.ArraySize := 1;
  Desc.Format := ColorFormat;
  Desc.SampleDesc.Count := 1;
  Desc.Usage := D3D11_USAGE_DEFAULT;
  Desc.BindFlags := D3D11_BIND_RENDER_TARGET;
  Check(FDev.Device.CreateTexture2D(Desc, nil, FResolved), 'CreateTexture2D(color)');
  if FDev.Samples > 1 then
  begin
    Desc.SampleDesc.Count := FDev.Samples;
    Check(FDev.Device.CreateTexture2D(Desc, nil, FColor), 'CreateTexture2D(MSAA color)');
  end
  else
    FColor := FResolved;
  Check(FDev.Device.CreateRenderTargetView(FColor, nil, FRTV), 'CreateRenderTargetView');

  Desc.Format := DepthFormat;
  Desc.BindFlags := D3D11_BIND_DEPTH_STENCIL;
  Check(FDev.Device.CreateTexture2D(Desc, nil, FDepth), 'CreateTexture2D(depth)');
  Check(FDev.Device.CreateDepthStencilView(FDepth, nil, FDSV), 'CreateDepthStencilView');
end;

procedure TRenderTarget.ReleaseBuffers;
var
  NoRTV: ID3D11RenderTargetView;
begin
  if FDev.Context <> nil then
    FDev.Context.OMSetRenderTargets(0, NoRTV, nil);
  FRTV := nil;
  FDSV := nil;
  FColor := nil;
  FResolved := nil;
  FDepth := nil;
end;

procedure TRenderTarget.Resize(Width, Height: Integer);
begin
  Width := Max(Width, 1);
  Height := Max(Height, 1);
  if (Width = FWidth) and (Height = FHeight) then
    Exit;
  ReleaseBuffers;
  FWidth := Width;
  FHeight := Height;
  if FSwapChain <> nil then
    Check(FSwapChain.ResizeBuffers(1, FWidth, FHeight, DXGI_FORMAT_R8G8B8A8_UNORM, 0),
      'ResizeBuffers');
  CreateBuffers;
end;

procedure TRenderTarget.BeginFrame(const ClearColor: TFourSingleArray);
var
  VP: D3D11_VIEWPORT;
begin
  FDev.Context.OMSetRenderTargets(1, FRTV, FDSV);
  FillChar(VP, SizeOf(VP), 0);
  VP.Width := FWidth;
  VP.Height := FHeight;
  VP.MaxDepth := 1;
  FDev.Context.RSSetViewports(1, @VP);
  FDev.Context.ClearRenderTargetView(FRTV, ClearColor);
  FDev.Context.ClearDepthStencilView(FDSV, D3D11_CLEAR_DEPTH or D3D11_CLEAR_STENCIL, 1, 0);
end;

procedure TRenderTarget.Resolve;
begin
  if FColor <> FResolved then
    FDev.Context.ResolveSubresource(FResolved, 0, FColor, 0, ColorFormat);
end;

function TRenderTarget.Present: Boolean;
var
  BackBuffer: ID3D11Texture2D;
  HR: HRESULT;
begin
  Resolve;
  if Failed(FSwapChain.GetBuffer(0, ID3D11Texture2D, BackBuffer)) then
    Exit(False);
  // UNORM back buffer, sRGB frame: same bytes, CopyResource allows it
  FDev.Context.CopyResource(BackBuffer, FResolved);
  BackBuffer := nil;
  HR := FSwapChain.Present(1, 0);
  Result := (HR <> DXGI_ERROR_DEVICE_REMOVED) and (HR <> DXGI_ERROR_DEVICE_RESET);
end;

function TRenderTarget.ReadPixels: TBytes;
var
  Desc: D3D11_TEXTURE2D_DESC;
  Staging: ID3D11Texture2D;
  Mapped: D3D11_MAPPED_SUBRESOURCE;
  Y: Integer;
begin
  Resolve;
  FResolved.GetDesc(Desc);
  Desc.Usage := D3D11_USAGE_STAGING;
  Desc.BindFlags := 0;
  Desc.CPUAccessFlags := D3D11_CPU_ACCESS_READ;
  Check(FDev.Device.CreateTexture2D(Desc, nil, Staging), 'CreateTexture2D(staging)');
  FDev.Context.CopyResource(Staging, FResolved);
  Check(FDev.Context.Map(Staging, 0, D3D11_MAP_READ, 0, Mapped), 'Map');
  try
    SetLength(Result, FWidth * FHeight * 4);
    for Y := 0 to FHeight - 1 do
      Move(PByte(Mapped.pData)[Y * Integer(Mapped.RowPitch)], Result[Y * FWidth * 4],
        FWidth * 4);
  finally
    FDev.Context.Unmap(Staging, 0);
  end;
  for Y := 0 to FWidth * FHeight - 1 do
    Result[Y * 4 + 3] := 255;
end;

{ WIC }

var
  WicFactory: IWICImagingFactory;

function Wic: IWICImagingFactory;
begin
  // COM must be initialized on this thread (CoInitializeEx)
  if WicFactory = nil then
    Check(CoCreateInstance(CLSID_WICImagingFactory, nil, CLSCTX_INPROC_SERVER,
      IWICImagingFactory, WicFactory), 'CoCreateInstance(WICImagingFactory)');
  Result := WicFactory;
end;

function DecodeImage(Data: Pointer; Size: NativeUInt; out Width, Height: Integer): TBytes;
var
  Stream: IWICStream;
  Decoder: IWICBitmapDecoder;
  Frame: IWICBitmapFrameDecode;
  Converter: IWICFormatConverter;
  W, H: UINT;
begin
  Check(Wic.CreateStream(Stream), 'IWICImagingFactory.CreateStream');
  Check(Stream.InitializeFromMemory(Data, Size), 'IWICStream.InitializeFromMemory');
  Check(Wic.CreateDecoderFromStream(Stream, GUID_NULL, WICDecodeMetadataCacheOnDemand,
    Decoder), 'CreateDecoderFromStream');
  Check(Decoder.GetFrame(0, Frame), 'IWICBitmapDecoder.GetFrame');
  Check(Wic.CreateFormatConverter(Converter), 'CreateFormatConverter');
  Check(Converter.Initialize(Frame, GUID_WICPixelFormat32bppRGBA, WICBitmapDitherTypeNone,
    nil, 0, WICBitmapPaletteTypeCustom), 'IWICFormatConverter.Initialize');
  Check(Converter.GetSize(W, H), 'GetSize');
  Width := W;
  Height := H;
  SetLength(Result, W * H * 4);
  Check(Converter.CopyPixels(nil, W * 4, Length(Result), PByte(Result)), 'CopyPixels');
end;

procedure SavePng(const FileName: string; const Pixels: TBytes; Width, Height: Integer);
var
  Stream: IWICStream;
  Encoder: IWICBitmapEncoder;
  Frame: IWICBitmapFrameEncode;
  Props: IPropertyBag2;
  Format: TGUID;
  Data: TBytes;
  I: Integer;
  T: Byte;
begin
  Check(Wic.CreateStream(Stream), 'IWICImagingFactory.CreateStream');
  Check(Stream.InitializeFromFilename(PChar(FileName), GENERIC_WRITE),
    'IWICStream.InitializeFromFilename');
  Check(Wic.CreateEncoder(GUID_ContainerFormatPng, GUID_NULL, Encoder), 'CreateEncoder');
  Check(Encoder.Initialize(Stream, WICBitmapEncoderNoCache), 'IWICBitmapEncoder.Initialize');
  Props := nil;
  Check(Encoder.CreateNewFrame(Frame, Props), 'CreateNewFrame');
  Check(Frame.Initialize(Props), 'IWICBitmapFrameEncode.Initialize');
  Check(Frame.SetSize(Width, Height), 'SetSize');
  Format := GUID_WICPixelFormat32bppRGBA;
  Check(Frame.SetPixelFormat(Format), 'SetPixelFormat');
  Data := Pixels;
  if Format = GUID_WICPixelFormat32bppBGRA then
  begin
    // Windows 7's encoder knows no RGBA
    Data := Copy(Pixels);
    for I := 0 to Width * Height - 1 do
    begin
      T := Data[I * 4];
      Data[I * 4] := Data[I * 4 + 2];
      Data[I * 4 + 2] := T;
    end;
  end
  else if Format <> GUID_WICPixelFormat32bppRGBA then
    raise EGraphics.Create('PNG encoder: no 32-bit RGBA or BGRA');
  Check(Frame.WritePixels(Height, Width * 4, Length(Data), PByte(Data)), 'WritePixels');
  Check(Frame.Commit, 'IWICBitmapFrameEncode.Commit');
  Check(Encoder.Commit, 'IWICBitmapEncoder.Commit');
end;

initialization

finalization
  WicFactory := nil;

end.
