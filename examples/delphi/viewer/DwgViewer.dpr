// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

{ DwgViewer: a Direct3D 11 viewer for DWG drawings on top of dwg2gltf.dll.

    DwgViewer [FILE.dwg] [--warp]
    DwgViewer FILE.dwg --screenshot OUT.png [--size WxH] [--view NAME]
      [--material NAME] [--ortho] [--select NODE] [--measure X1,Y1,X2,Y2] [--warp]

  --warp        use WARP (software rendering) even when a GPU is there
  --screenshot  render one frame per mode offscreen into OUT-<mode>.png and exit
                (no window; prints what it did on standard output)
  --size        image size, default 960x720
  --view        front, back, left, right, top, bottom or iso (default)
  --material    drawing (default), clay, metal or glass
  --ortho       orthographic projection
  --select      highlight the instances below node NODE (as the block tree does)
  --measure     snap at two pixels and measure, drawn into OUT-measure.png

  Needs dwg2gltf.dll next to the executable; everything else is Windows'
  own (d3d11, dxgi, d3dcompiler_47, windowscodecs, comctl32). }
program DwgViewer;

{$APPTYPE GUI}

uses
  Winapi.Windows,
  Winapi.ActiveX,
  System.SysUtils,
  System.Math,
  System.Diagnostics,
  Dwg2Gltf in '..\..\..\bindings\delphi\Dwg2Gltf.pas',
  Dwg2Gltf.Math in 'Dwg2Gltf.Math.pas',
  Dwg2Gltf.Camera in 'Dwg2Gltf.Camera.pas',
  Dwg2Gltf.D3D11 in 'Dwg2Gltf.D3D11.pas',
  Dwg2Gltf.Renderer in 'Dwg2Gltf.Renderer.pas',
  Dwg2Gltf.Picking in 'Dwg2Gltf.Picking.pas',
  ViewerDocument in 'ViewerDocument.pas',
  ViewerWindow in 'ViewerWindow.pas';

const
  ModeFileNames: array[TRenderMode] of string =
    ('shaded', 'edges', 'hidden', 'wireframe', 'triangles', 'xray');

var
  LogHandle: THandle = 0;

{ A GUI program has no console of its own: write to the standard output
  the caller redirected, else to the console of the command prompt that
  started it. }
procedure Log(const S: string);
var
  Utf8: UTF8String;
  Written: DWORD;
  H: THandle;
begin
  if LogHandle = 0 then
  begin
    LogHandle := GetStdHandle(STD_OUTPUT_HANDLE);
    if ((LogHandle = 0) or (LogHandle = INVALID_HANDLE_VALUE)) and
      AttachConsole(ATTACH_PARENT_PROCESS) then
      LogHandle := CreateFile('CONOUT$', GENERIC_WRITE, FILE_SHARE_WRITE, nil,
        OPEN_EXISTING, 0, 0);
  end;
  H := LogHandle;
  if (H = 0) or (H = INVALID_HANDLE_VALUE) then
    Exit;
  Utf8 := UTF8String(S + sLineBreak);
  WriteFile(H, PAnsiChar(Utf8)^, Length(Utf8), Written, nil);
end;

function Option(const Name: string; out Value: string): Boolean;
var
  I: Integer;
begin
  for I := 1 to ParamCount - 1 do
    if ParamStr(I) = Name then
    begin
      Value := ParamStr(I + 1);
      Exit(True);
    end;
  Result := False;
end;

function Flag(const Name: string): Boolean;
begin
  Result := FindCmdLineSwitch(Copy(Name, 3, MaxInt), ['-'], True);
end;

function MatchStr(const S: string; const Values: array of string): Boolean;
var
  V: string;
begin
  for V in Values do
    if S = V then
      Exit(True);
  Result := False;
end;

{ The first argument that is not an option or an option's value. }
function InputFile: string;
var
  I: Integer;
begin
  I := 1;
  while I <= ParamCount do
  begin
    if ParamStr(I).StartsWith('--') then
    begin
      if not MatchStr(ParamStr(I), ['--warp', '--ortho']) then
        Inc(I); // skip the value
    end
    else
      Exit(ParamStr(I));
    Inc(I);
  end;
  Result := '';
end;

function ParseView(const S: string): TStandardView;
const
  Names: array[TStandardView] of string = ('front', 'back', 'left', 'right', 'top', 'bottom', 'iso');
var
  V: TStandardView;
begin
  for V := Low(V) to High(V) do
    if SameText(S, Names[V]) then
      Exit(V);
  raise Exception.Create('unknown view: ' + S);
end;

function ParseMaterial(const S: string): TMaterialPreset;
const
  Names: array[TMaterialPreset] of string = ('drawing', 'clay', 'metal', 'glass');
var
  M: TMaterialPreset;
begin
  for M := Low(M) to High(M) do
    if SameText(S, Names[M]) then
      Exit(M);
  raise Exception.Create('unknown material: ' + S);
end;

procedure RunScreenshots(const FileName, OutFile: string);
var
  Dev: TD3DDevice;
  Doc: TViewerDocument;
  Renderer: TDwgRenderer;
  Target: TRenderTarget;
  Camera: TOrbitCamera;
  Mode: TRenderMode;
  S, Base: string;
  W, H, I: Integer;
  Parts: TArray<string>;
  Snaps: array[0..1] of TSnap;
  Line: TOverlayLine;
  Marker: TOverlayMarker;
  Fmt: TFormatSettings;
  Watch: TStopwatch;
begin
  Fmt := TFormatSettings.Invariant;
  W := 960;
  H := 720;
  if Option('--size', S) then
  begin
    Parts := S.Split(['x']);
    W := StrToInt(Parts[0]);
    H := StrToInt(Parts[1]);
  end;
  Base := ChangeFileExt(OutFile, '');
  Dev := TD3DDevice.Create(Flag('--warp'));
  Doc := nil;
  Renderer := nil;
  Target := nil;
  try
    Log(Dev.Description);
    Doc := TViewerDocument.Create(FileName);
    Log(Format('%s: %d meshes, %d instances, %d materials, %d textures, %d edges, converted in %.2f s',
      [ExtractFileName(FileName), Doc.Data.mesh_count, Doc.Data.instance_count,
       Doc.Data.material_count, Doc.Data.texture_count, Doc.Data.edge_count, Doc.LoadSeconds], Fmt));
    Renderer := TDwgRenderer.Create(Dev);
    Renderer.Upload(Doc.Data^, Doc.Origin);
    Log(Format('uploaded %.1f KB to the GPU', [Renderer.UploadBytes / 1024], Fmt));
    for S in Renderer.Warnings do
      Log('warning: ' + S);
    Target := TRenderTarget.Create(Dev, 0, W, H);

    Camera.Init;
    Camera.Width := W;
    Camera.Height := H;
    Camera.SceneBox := Doc.Box;
    Camera.Ortho := Flag('--ortho');
    if Option('--view', S) then
      Camera.SetView(ParseView(S));
    Camera.Fit(Doc.Box);
    if Option('--material', S) then
      Renderer.Preset := ParseMaterial(S);
    if Option('--select', S) then
      Renderer.SetHighlight(Doc.SubtreeInstances(StrToInt(S)));

    for Mode := Low(TRenderMode) to High(TRenderMode) do
    begin
      Renderer.Mode := Mode;
      Renderer.Render(Target, Camera);
      SavePng(Base + '-' + ModeFileNames[Mode] + '.png', Target.ReadPixels, W, H);
      Log(Format('%s: %s, %d draw calls',
        [Base + '-' + ModeFileNames[Mode] + '.png', RenderModeNames[Mode], Renderer.DrawCalls]));
    end;

    // frame time: 20 frames, waiting for the last one (ReadPixels)
    Renderer.Mode := rmShadedEdges;
    Renderer.Render(Target, Camera);
    Target.ReadPixels;
    Watch := TStopwatch.StartNew;
    for I := 1 to 20 do
      Renderer.Render(Target, Camera);
    Target.ReadPixels;
    Log(Format('shaded with edges: %.1f ms per frame', [Watch.Elapsed.TotalMilliseconds / 20], Fmt));

    if Option('--measure', S) then
    begin
      Parts := S.Split([',']);
      for I := 0 to 1 do
      begin
        Watch := TStopwatch.StartNew;
        Snaps[I] := Doc.Picker.Snap(Camera, StrToFloat(Parts[2 * I], Fmt),
          StrToFloat(Parts[2 * I + 1], Fmt), 12);
        Log(Format('snap %d took %.1f ms', [I + 1, Watch.Elapsed.TotalMilliseconds], Fmt));
        if Snaps[I].Kind = skNone then
          raise Exception.CreateFmt('nothing at pixel %s,%s', [Parts[2 * I], Parts[2 * I + 1]]);
        Log(Format('snap %d: %s at (%.4f, %.4f, %.4f) m, instance %d',
          [I + 1, SnapKindNames[Snaps[I].Kind], Snaps[I].Point.X + Doc.Origin.X,
           Snaps[I].Point.Y + Doc.Origin.Y, Snaps[I].Point.Z + Doc.Origin.Z, Snaps[I].Instance], Fmt));
        Marker.P := Snaps[I].Point;
        Marker.Kind := TMarkerKind(Ord(Snaps[I].Kind));
        Marker.Color := Color4(0.0, 0.85, 0.25);
        Renderer.Markers := Renderer.Markers + [Marker];
      end;
      Log(Format('distance: %.4f m', [(Snaps[1].Point - Snaps[0].Point).Length], Fmt));
      Line.A := Snaps[0].Point;
      Line.B := Snaps[1].Point;
      Line.Color := Color4(0.0, 0.85, 0.25);
      Renderer.Overlay := [Line];
      Renderer.Mode := rmShadedEdges;
      Renderer.Render(Target, Camera);
      SavePng(Base + '-measure.png', Target.ReadPixels, W, H);
      Log(Base + '-measure.png');
    end;
  finally
    Target.Free;
    Renderer.Free;
    Doc.Free;
    Dev.Free;
  end;
end;

var
  OutFile: string;
begin
  CoInitializeEx(nil, COINIT_APARTMENTTHREADED);
  try
    if Option('--screenshot', OutFile) then
    begin
      try
        if InputFile = '' then
          raise Exception.Create('--screenshot needs a drawing');
        RunScreenshots(InputFile, OutFile);
      except
        on E: Exception do
        begin
          Log('error: ' + E.Message);
          ExitCode := 1;
        end;
      end;
    end
    else
      RunViewer(InputFile, Flag('--warp'));
  finally
    CoUninitialize;
  end;
end.
