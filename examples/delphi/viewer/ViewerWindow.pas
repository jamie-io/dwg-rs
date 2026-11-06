// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

{ ViewerWindow - the viewer's window in plain Win32: a menu, the block tree
  (a tree view), the 3D view (a child window Direct3D presents into) and a
  status bar.

  The rendering units only need a window handle and its size, so in a VCL
  application the 3D view can be any TWinControl (a TPanel): create the
  TRenderTarget with Panel.Handle, call TRenderTarget.Resize from OnResize,
  render and present from a WM_PAINT handler (or Application.OnIdle when
  something changed), and route the panel's mouse events to the camera and
  picker as below. }
unit ViewerWindow;

interface

procedure RunViewer(const FileName: string; ForceWarp: Boolean);

implementation

uses
  Winapi.Windows, Winapi.Messages, Winapi.CommCtrl, Winapi.CommDlg, Winapi.ShellAPI,
  System.SysUtils, System.Math, System.Types,
  Dwg2Gltf, Dwg2Gltf.Math, Dwg2Gltf.Camera, Dwg2Gltf.D3D11, Dwg2Gltf.Renderer,
  Dwg2Gltf.Picking, ViewerDocument;

const
  IDM_OPEN = 100;
  IDM_EXIT = 101;
  IDM_MODE = 200;   // + TRenderMode
  IDM_PRESET = 300; // + TMaterialPreset
  IDM_VIEW = 400;   // + TStandardView
  IDM_FIT = 420;
  IDM_ORTHO = 421;
  IDM_MEASURE = 500;
  IDM_CLEAR = 501;

  TreeWidth = 300;
  SnapRadius = 12; // pixels
  MeasureColor: TColor4 = (0.0, 0.85, 0.25, 1);
  HoverColor: TColor4 = (1.0, 0.85, 0.0, 1);

type
  TDrag = (dgNone, dgOrbit, dgPan);

  TViewer = class
  private
    FMain, FTree, FView, FStatus: HWND;
    FMenu: HMENU;
    FDev: TD3DDevice;
    FTarget: TRenderTarget;
    FRenderer: TDwgRenderer;
    FDoc: TViewerDocument;
    FCamera: TOrbitCamera;
    FForceWarp: Boolean;
    FBroken: Boolean; // rendering failed: stop trying
    FDrag: TDrag;
    FDragged: Boolean;
    FDown, FLast: TPoint;
    FMeasuring: Boolean;
    FPoints: TArray<TSnap>; // measured points: 0, 1 or 2
    FHover: TSnap;
    FNodeItems: TArray<HTREEITEM>;
    FFilled: TArray<Boolean>; // tree item's children inserted
    procedure CreateWindows;
    procedure CreateGraphics;
    procedure FreeGraphics;
    procedure Layout;
    procedure Open(const FileName: string);
    procedure OpenDialog;
    procedure Command(Id: Integer);
    procedure UpdateMenu;
    procedure SetStatus(Part: Integer; const Text: string);
    procedure UpdateOverlay;
    procedure Paint;
    procedure Invalidate;
    procedure SyncCameraSize;
    procedure Fit;
    // block tree
    procedure FillTree;
    function AddTreeItem(Parent: HTREEITEM; Node: Integer): HTREEITEM;
    procedure FillChildren(Node: Integer);
    procedure SelectNode(Node: Integer);
    procedure TreeSelected(Node: Integer);
    // mouse in the 3D view
    procedure MouseDown(X, Y: Integer; Drag: TDrag);
    procedure MouseMove(X, Y: Integer);
    procedure MouseUp(X, Y: Integer);
    procedure Click(X, Y: Integer);
    procedure Wheel(Delta, X, Y: Integer);
    function FormatPoint(const P: TVec3): string;
  public
    constructor Create(ForceWarp: Boolean);
    destructor Destroy; override;
  end;

var
  Viewer: TViewer;

function MainProc(Wnd: HWND; Msg: UINT; WParam: WPARAM; LParam: LPARAM): LRESULT; stdcall;
var
  Notify: PNMTreeView;
  Files: HDROP;
  Name: array[0..MAX_PATH] of Char;
begin
  Result := 0;
  case Msg of
    WM_SIZE:
      if Viewer <> nil then
        Viewer.Layout;
    WM_COMMAND:
      if Viewer <> nil then
        Viewer.Command(LoWord(WParam));
    WM_NOTIFY:
      begin
        Notify := PNMTreeView(LParam);
        if (Viewer <> nil) and (Notify.hdr.hwndFrom = Viewer.FTree) then
          case Integer(Notify.hdr.code) of
            TVN_ITEMEXPANDING:
              if Notify.action and TVE_EXPAND <> 0 then
                Viewer.FillChildren(Notify.itemNew.lParam);
            TVN_SELCHANGED:
              if Notify.itemNew.hItem <> nil then
                Viewer.TreeSelected(Notify.itemNew.lParam);
          end;
      end;
    WM_DROPFILES:
      begin
        Files := HDROP(WParam);
        if (Viewer <> nil) and (DragQueryFile(Files, 0, Name, Length(Name)) > 0) then
          Viewer.Open(Name);
        DragFinish(Files);
      end;
    WM_DESTROY:
      PostQuitMessage(0);
  else
    Result := DefWindowProc(Wnd, Msg, WParam, LParam);
  end;
end;

function ViewProc(Wnd: HWND; Msg: UINT; WParam: WPARAM; LParam: LPARAM): LRESULT; stdcall;
var
  X, Y: Integer;
  P: TPoint;
  PS: TPaintStruct;
begin
  Result := 0;
  if Viewer = nil then
    Exit(DefWindowProc(Wnd, Msg, WParam, LParam));
  X := SmallInt(LoWord(LParam));
  Y := SmallInt(HiWord(LParam));
  case Msg of
    WM_PAINT:
      begin
        BeginPaint(Wnd, PS);
        Viewer.Paint;
        EndPaint(Wnd, PS);
      end;
    WM_ERASEBKGND:
      Result := 1; // Direct3D paints everything
    WM_SIZE:
      Viewer.Invalidate; // Paint resizes the target
    WM_LBUTTONDOWN:
      if WParam and MK_SHIFT <> 0 then
        Viewer.MouseDown(X, Y, dgPan)
      else
        Viewer.MouseDown(X, Y, dgOrbit);
    WM_MBUTTONDOWN, WM_RBUTTONDOWN:
      Viewer.MouseDown(X, Y, dgPan);
    WM_MOUSEMOVE:
      Viewer.MouseMove(X, Y);
    WM_LBUTTONUP, WM_MBUTTONUP, WM_RBUTTONUP:
      Viewer.MouseUp(X, Y);
    WM_MOUSEWHEEL:
      begin
        P := Point(X, Y); // screen coordinates for this message
        ScreenToClient(Wnd, P);
        Viewer.Wheel(SmallInt(HiWord(WParam)), P.X, P.Y);
      end;
  else
    Result := DefWindowProc(Wnd, Msg, WParam, LParam);
  end;
end;

{ TViewer }

constructor TViewer.Create(ForceWarp: Boolean);
begin
  inherited Create;
  FForceWarp := ForceWarp;
  FCamera.Init;
  CreateWindows;
  CreateGraphics;
  UpdateMenu;
end;

destructor TViewer.Destroy;
begin
  FreeGraphics;
  FDoc.Free;
  inherited;
end;

procedure TViewer.CreateWindows;
const
  ViewNames: array[TStandardView] of string =
    ('&Front', '&Back', '&Left', '&Right', '&Top', 'Botto&m', '&Isometric');
var
  WC: TWndClassEx;
  ICC: TInitCommonControlsEx;
  Sub: HMENU;
  Mode: TRenderMode;
  Preset: TMaterialPreset;
  View: TStandardView;
begin
  ICC.dwSize := SizeOf(ICC);
  ICC.dwICC := ICC_TREEVIEW_CLASSES or ICC_BAR_CLASSES;
  InitCommonControlsEx(ICC);

  FillChar(WC, SizeOf(WC), 0);
  WC.cbSize := SizeOf(WC);
  WC.lpfnWndProc := @MainProc;
  WC.hInstance := HInstance;
  WC.hCursor := LoadCursor(0, IDC_ARROW);
  WC.hIcon := LoadIcon(0, IDI_APPLICATION);
  WC.hbrBackground := COLOR_BTNFACE + 1;
  WC.lpszClassName := 'DwgViewerMain';
  RegisterClassEx(WC);
  WC.lpfnWndProc := @ViewProc;
  WC.hbrBackground := 0;
  WC.lpszClassName := 'DwgViewer3D';
  RegisterClassEx(WC);

  FMenu := CreateMenu;
  Sub := CreatePopupMenu;
  AppendMenu(Sub, MF_STRING, IDM_OPEN, '&Open...'#9'Ctrl+O');
  AppendMenu(Sub, MF_SEPARATOR, 0, nil);
  AppendMenu(Sub, MF_STRING, IDM_EXIT, 'E&xit');
  AppendMenu(FMenu, MF_POPUP, Sub, '&File');
  Sub := CreatePopupMenu;
  for Mode := Low(Mode) to High(Mode) do
    AppendMenu(Sub, MF_STRING, IDM_MODE + Ord(Mode),
      PChar(Format('%s'#9'%d', [RenderModeNames[Mode], Ord(Mode) + 1])));
  AppendMenu(FMenu, MF_POPUP, Sub, '&Mode');
  Sub := CreatePopupMenu;
  for Preset := Low(Preset) to High(Preset) do
    AppendMenu(Sub, MF_STRING, IDM_PRESET + Ord(Preset), PChar(MaterialPresetNames[Preset]));
  AppendMenu(FMenu, MF_POPUP, Sub, 'M&aterial');
  Sub := CreatePopupMenu;
  AppendMenu(Sub, MF_STRING, IDM_FIT, '&Fit'#9'F');
  AppendMenu(Sub, MF_STRING, IDM_ORTHO, '&Orthographic'#9'O');
  AppendMenu(Sub, MF_SEPARATOR, 0, nil);
  for View := Low(View) to High(View) do
    AppendMenu(Sub, MF_STRING, IDM_VIEW + Ord(View), PChar(ViewNames[View]));
  AppendMenu(FMenu, MF_POPUP, Sub, '&View');
  Sub := CreatePopupMenu;
  AppendMenu(Sub, MF_STRING, IDM_MEASURE, '&Measure distance'#9'M');
  AppendMenu(Sub, MF_STRING, IDM_CLEAR, '&Clear'#9'Esc');
  AppendMenu(FMenu, MF_POPUP, Sub, '&Tools');

  FMain := CreateWindowEx(WS_EX_ACCEPTFILES, 'DwgViewerMain', 'DwgViewer',
    WS_OVERLAPPEDWINDOW or WS_CLIPCHILDREN, Integer(CW_USEDEFAULT), Integer(CW_USEDEFAULT),
    1280, 860, 0, FMenu, HInstance, nil);
  FTree := CreateWindowEx(WS_EX_CLIENTEDGE, WC_TREEVIEW, nil,
    WS_CHILD or WS_VISIBLE or TVS_HASLINES or TVS_HASBUTTONS or TVS_LINESATROOT or
    TVS_SHOWSELALWAYS, 0, 0, TreeWidth, 100, FMain, 0, HInstance, nil);
  FView := CreateWindowEx(0, 'DwgViewer3D', nil, WS_CHILD or WS_VISIBLE,
    TreeWidth, 0, 100, 100, FMain, 0, HInstance, nil);
  FStatus := CreateWindowEx(0, STATUSCLASSNAME, nil, WS_CHILD or WS_VISIBLE or SBARS_SIZEGRIP,
    0, 0, 0, 0, FMain, 0, HInstance, nil);
  SendMessage(FTree, WM_SETFONT, GetStockObject(DEFAULT_GUI_FONT), 0);
end;

procedure TViewer.CreateGraphics;
var
  R: TRect;
begin
  GetClientRect(FView, R);
  FDev := TD3DDevice.Create(FForceWarp);
  FTarget := TRenderTarget.Create(FDev, FView, R.Width, R.Height);
  FRenderer := TDwgRenderer.Create(FDev);
  if FDoc <> nil then
    FRenderer.Upload(FDoc.Data^, FDoc.Origin);
  SetStatus(0, FDev.Description);
end;

procedure TViewer.FreeGraphics;
begin
  FreeAndNil(FRenderer);
  FreeAndNil(FTarget);
  FreeAndNil(FDev);
end;

procedure TViewer.Layout;
var
  R, SR: TRect;
  H: Integer;
  Parts: array[0..2] of Integer;
begin
  GetClientRect(FMain, R);
  // status bar: device | scene, selection or snap | measurement
  Parts[0] := R.Width * 3 div 10;
  Parts[1] := R.Width * 6 div 10;
  Parts[2] := -1;
  SendMessage(FStatus, SB_SETPARTS, Length(Parts), Winapi.Windows.LPARAM(@Parts));
  SendMessage(FStatus, WM_SIZE, 0, 0);
  GetWindowRect(FStatus, SR);
  H := SR.Height;
  GetClientRect(FMain, R);
  MoveWindow(FTree, 0, 0, TreeWidth, R.Height - H, True);
  MoveWindow(FView, TreeWidth, 0, Max(R.Width - TreeWidth, 1), Max(R.Height - H, 1), True);
end;

procedure TViewer.Invalidate;
begin
  InvalidateRect(FView, nil, False);
end;

procedure TViewer.SyncCameraSize;
var
  R: TRect;
begin
  GetClientRect(FView, R);
  FCamera.Width := Max(R.Width, 1);
  FCamera.Height := Max(R.Height, 1);
end;

procedure TViewer.Fit;
begin
  if FDoc = nil then
    Exit;
  SyncCameraSize; // the fit depends on the aspect ratio
  FCamera.Fit(FDoc.Box);
end;

procedure TViewer.SetStatus(Part: Integer; const Text: string);
begin
  SendMessage(FStatus, SB_SETTEXT, Part, Winapi.Windows.LPARAM(PChar(Text)));
end;

function TViewer.FormatPoint(const P: TVec3): string;
var
  W: TVec3;
begin
  // the drawing's axes (Z up) in millimeters: undo the viewer's origin
  // and the Y-up turn of the glTF frame
  W := P + FDoc.Origin;
  Result := Format('%.1f, %.1f, %.1f mm', [W.X * 1000, -W.Z * 1000, W.Y * 1000]);
end;

procedure TViewer.Open(const FileName: string);
var
  Doc: TViewerDocument;
  OldCursor: HCURSOR;
begin
  OldCursor := SetCursor(LoadCursor(0, IDC_WAIT));
  try
    // conversion runs on the calling thread here; a production viewer
    // would open on a worker thread (the DLL allows it) and upload after
    try
      Doc := TViewerDocument.Create(FileName);
    except
      on E: Exception do
      begin
        MessageBox(FMain, PChar(E.Message), 'DwgViewer', MB_ICONERROR);
        Exit;
      end;
    end;
    FRenderer.Clear;
    FDoc.Free;
    FDoc := Doc;
    FRenderer.Upload(FDoc.Data^, FDoc.Origin);
  finally
    SetCursor(OldCursor);
  end;
  FPoints := nil;
  FHover.Kind := skNone;
  UpdateOverlay;
  FCamera.SceneBox := FDoc.Box;
  FCamera.SetView(svIso);
  Fit;
  FillTree;
  SetWindowText(FMain, PChar('DwgViewer - ' + ExtractFileName(FileName)));
  SetStatus(1, Format('%d instances, %d triangles drawn, %.1f MB on the GPU, %.2f s',
    [FDoc.Data.instance_count, FDoc.Data.triangles_drawn, FRenderer.UploadBytes / 1048576,
     FDoc.LoadSeconds]));
  SetStatus(2, '');
  Invalidate;
end;

procedure TViewer.OpenDialog;
var
  OFN: TOpenFilename;
  Name: array[0..MAX_PATH] of Char;
begin
  FillChar(OFN, SizeOf(OFN), 0);
  FillChar(Name, SizeOf(Name), 0);
  OFN.lStructSize := SizeOf(OFN);
  OFN.hWndOwner := FMain;
  OFN.lpstrFilter := 'DWG drawings (*.dwg)'#0'*.dwg'#0'All files'#0'*.*'#0#0;
  OFN.lpstrFile := Name;
  OFN.nMaxFile := Length(Name);
  OFN.Flags := OFN_FILEMUSTEXIST or OFN_HIDEREADONLY;
  if GetOpenFileName(OFN) then
    Open(Name);
end;

procedure TViewer.Command(Id: Integer);
begin
  case Id of
    IDM_OPEN: OpenDialog;
    IDM_EXIT: DestroyWindow(FMain);
    IDM_MODE .. IDM_MODE + Ord(High(TRenderMode)):
      FRenderer.Mode := TRenderMode(Id - IDM_MODE);
    IDM_PRESET .. IDM_PRESET + Ord(High(TMaterialPreset)):
      FRenderer.Preset := TMaterialPreset(Id - IDM_PRESET);
    IDM_VIEW .. IDM_VIEW + Ord(High(TStandardView)):
      begin
        FCamera.SetView(TStandardView(Id - IDM_VIEW));
        Fit;
      end;
    IDM_FIT:
      Fit;
    IDM_ORTHO:
      begin
        FCamera.Ortho := not FCamera.Ortho;
        Fit;
      end;
    IDM_MEASURE:
      begin
        FMeasuring := not FMeasuring;
        FPoints := nil;
        FHover.Kind := skNone;
        if FMeasuring then
          SetStatus(2, 'Measure: click two points')
        else
          SetStatus(2, '');
        UpdateOverlay;
      end;
    IDM_CLEAR:
      begin
        FPoints := nil;
        FHover.Kind := skNone;
        FMeasuring := False;
        SetStatus(2, '');
        TreeView_SelectItem(FTree, nil);
        FRenderer.SetHighlight(nil);
        UpdateOverlay;
      end;
  end;
  UpdateMenu;
  Invalidate;
end;

procedure TViewer.UpdateMenu;
const
  Checked: array[Boolean] of UINT = (MF_UNCHECKED, MF_CHECKED);
var
  Mode: TRenderMode;
  Preset: TMaterialPreset;
begin
  if FRenderer = nil then
    Exit;
  for Mode := Low(Mode) to High(Mode) do
    CheckMenuItem(FMenu, IDM_MODE + Ord(Mode), Checked[FRenderer.Mode = Mode]);
  for Preset := Low(Preset) to High(Preset) do
    CheckMenuItem(FMenu, IDM_PRESET + Ord(Preset), Checked[FRenderer.Preset = Preset]);
  CheckMenuItem(FMenu, IDM_ORTHO, Checked[FCamera.Ortho]);
  CheckMenuItem(FMenu, IDM_MEASURE, Checked[FMeasuring]);
end;

procedure TViewer.UpdateOverlay;
var
  Markers: TArray<TOverlayMarker>;
  Lines: TArray<TOverlayLine>;
  M: TOverlayMarker;
  L: TOverlayLine;
  P: TSnap;
begin
  for P in FPoints do
  begin
    M.P := P.Point;
    M.Kind := TMarkerKind(Ord(P.Kind));
    M.Color := MeasureColor;
    Markers := Markers + [M];
  end;
  if Length(FPoints) = 2 then
  begin
    L.A := FPoints[0].Point;
    L.B := FPoints[1].Point;
    L.Color := MeasureColor;
    Lines := [L];
  end
  else if (Length(FPoints) = 1) and (FHover.Kind <> skNone) then
  begin
    // rubber band from the first point to the cursor's snap
    L.A := FPoints[0].Point;
    L.B := FHover.Point;
    L.Color := HoverColor;
    Lines := [L];
  end;
  if FMeasuring and (FHover.Kind <> skNone) then
  begin
    M.P := FHover.Point;
    M.Kind := TMarkerKind(Ord(FHover.Kind));
    M.Color := HoverColor;
    Markers := Markers + [M];
  end;
  if FRenderer <> nil then
  begin
    FRenderer.Markers := Markers;
    FRenderer.Overlay := Lines;
  end;
end;

procedure TViewer.Paint;
var
  Lost: Boolean;
begin
  if (FTarget = nil) or FBroken then
    Exit;
  SyncCameraSize;
  try
    FTarget.Resize(FCamera.Width, FCamera.Height);
    FRenderer.Render(FTarget, FCamera);
    Lost := not FTarget.Present;
  except
    on E: EGraphics do
    begin
      // a call failed: because the device is gone, or for real
      Lost := Failed(FDev.Device.GetDeviceRemovedReason);
      if not Lost then
      begin
        FBroken := True; // no endless message boxes
        MessageBox(FMain, PChar(E.Message), 'DwgViewer', MB_ICONERROR);
        Exit;
      end;
    end;
  end;
  if Lost then
  begin
    // the device is gone (driver update, remote session switched to
    // another adapter): start over with a new one, the scene is still open
    FreeGraphics;
    CreateGraphics;
    UpdateOverlay;
    Invalidate;
  end;
end;

{ block tree }

function TViewer.AddTreeItem(Parent: HTREEITEM; Node: Integer): HTREEITEM;
var
  Ins: TTVInsertStruct;
  Text: string;
begin
  Text := FDoc.NodeLabel(Node);
  FillChar(Ins, SizeOf(Ins), 0);
  Ins.hParent := Parent;
  Ins.hInsertAfter := TVI_LAST;
  Ins.item.mask := TVIF_TEXT or TVIF_PARAM or TVIF_CHILDREN;
  Ins.item.pszText := PChar(Text);
  Ins.item.lParam := Node;
  // children are inserted when the item is first expanded: a drawing can
  // have a hundred thousand block instances
  Ins.item.cChildren := Ord(FDoc.FirstChild[Node] >= 0);
  Result := TreeView_InsertItem(FTree, Ins);
  FNodeItems[Node] := Result;
end;

procedure TViewer.FillTree;
var
  I: Integer;
begin
  TreeView_DeleteAllItems(FTree);
  SetLength(FNodeItems, FDoc.Data.node_count);
  SetLength(FFilled, FDoc.Data.node_count);
  for I := 0 to High(FNodeItems) do
  begin
    FNodeItems[I] := nil;
    FFilled[I] := False;
  end;
  for I := 0 to High(FNodeItems) do
    if FDoc.Data.nodes[I].parent < 0 then
    begin
      AddTreeItem(TVI_ROOT, I);
      FillChildren(I);
      TreeView_Expand(FTree, FNodeItems[I], TVE_EXPAND);
    end;
end;

procedure TViewer.FillChildren(Node: Integer);
var
  Child: Integer;
begin
  if (Node < 0) or (Node >= Length(FFilled)) or FFilled[Node] then
    Exit;
  FFilled[Node] := True;
  SendMessage(FTree, WM_SETREDRAW, 0, 0);
  Child := FDoc.FirstChild[Node];
  while Child >= 0 do
  begin
    AddTreeItem(FNodeItems[Node], Child);
    Child := FDoc.NextSibling[Child];
  end;
  SendMessage(FTree, WM_SETREDRAW, 1, 0);
end;

procedure TViewer.SelectNode(Node: Integer);
var
  Path: TArray<Integer>;
  I: Integer;
begin
  // expand the way down from the root, which inserts the items
  I := Node;
  while I >= 0 do
  begin
    Path := [I] + Path;
    I := FDoc.Data.nodes[I].parent;
  end;
  for I := 0 to High(Path) - 1 do
  begin
    FillChildren(Path[I]);
    TreeView_Expand(FTree, FNodeItems[Path[I]], TVE_EXPAND);
  end;
  TreeView_SelectItem(FTree, FNodeItems[Node]);
  TreeView_EnsureVisible(FTree, FNodeItems[Node]);
end;

procedure TViewer.TreeSelected(Node: Integer);
var
  Flags: TArray<Boolean>;
  Count, I: Integer;
begin
  Flags := FDoc.SubtreeInstances(Node);
  // the root holds everything: selecting it highlights nothing
  if FDoc.Data.nodes[Node].parent < 0 then
    Flags := nil;
  FRenderer.SetHighlight(Flags);
  Count := 0;
  for I := 0 to High(Flags) do
    Inc(Count, Ord(Flags[I]));
  SetStatus(1, Format('%s: %d instances', [FDoc.NodeLabel(Node), Count]));
  Invalidate;
end;

{ mouse }

procedure TViewer.MouseDown(X, Y: Integer; Drag: TDrag);
begin
  SetFocus(FView);
  SetCapture(FView);
  FDrag := Drag;
  FDragged := False;
  FDown := Point(X, Y);
  FLast := FDown;
end;

procedure TViewer.MouseMove(X, Y: Integer);
var
  Snap: TSnap;
begin
  if FDrag <> dgNone then
  begin
    // a click that moves a little is still a click
    if (Abs(X - FDown.X) > 3) or (Abs(Y - FDown.Y) > 3) then
      FDragged := True;
    if FDragged then
    begin
      if FDrag = dgOrbit then
        FCamera.Orbit(X - FLast.X, Y - FLast.Y)
      else
        FCamera.Pan(X - FLast.X, Y - FLast.Y);
      FLast := Point(X, Y);
      Invalidate;
    end;
  end
  else if FMeasuring and (FDoc <> nil) then
  begin
    Snap := FDoc.Picker.Snap(FCamera, X, Y, SnapRadius);
    if (Snap.Kind <> FHover.Kind) or ((Snap.Point - FHover.Point).Length > 0) then
    begin
      FHover := Snap;
      if Snap.Kind <> skNone then
        SetStatus(1, Format('%s: %s', [SnapKindNames[Snap.Kind], FormatPoint(Snap.Point)]))
      else
        SetStatus(1, '');
      UpdateOverlay;
      Invalidate;
    end;
  end;
end;

procedure TViewer.MouseUp(X, Y: Integer);
var
  WasClick: Boolean;
begin
  WasClick := (FDrag = dgOrbit) and not FDragged;
  FDrag := dgNone;
  ReleaseCapture;
  if WasClick and (FDoc <> nil) then
    Click(X, Y);
end;

procedure TViewer.Click(X, Y: Integer);
var
  Snap: TSnap;
  Hit: THit;
  D: TVec3;
  Node: Integer;
begin
  if FMeasuring then
  begin
    Snap := FDoc.Picker.Snap(FCamera, X, Y, SnapRadius);
    if Snap.Kind = skNone then
      Exit;
    if Length(FPoints) = 2 then
      FPoints := nil;
    FPoints := FPoints + [Snap];
    if Length(FPoints) = 2 then
    begin
      D := FPoints[1].Point - FPoints[0].Point;
      // differences along the drawing's axes (Z up)
      SetStatus(2, Format('Distance %.1f mm  (dx %.1f, dy %.1f, dz %.1f)',
        [D.Length * 1000, D.X * 1000, -D.Z * 1000, D.Y * 1000]));
    end
    else
      SetStatus(2, 'Measure: click the second point');
    UpdateOverlay;
    Invalidate;
    Exit;
  end;
  // select the block instance right below the model space that holds the hit
  if FDoc.Picker.Pick(FCamera.RayAt(X, Y), Hit) then
  begin
    Node := FRenderer.Instances[Hit.Instance].Node;
    while (FDoc.Data.nodes[Node].parent >= 0) and
      (FDoc.Data.nodes[FDoc.Data.nodes[Node].parent].parent >= 0) do
      Node := FDoc.Data.nodes[Node].parent;
    SelectNode(Node);
  end
  else
    Command(IDM_CLEAR);
end;

procedure TViewer.Wheel(Delta, X, Y: Integer);
begin
  FCamera.Zoom(Power(0.85, Delta / WHEEL_DELTA), X, Y);
  Invalidate;
end;

procedure RunViewer(const FileName: string; ForceWarp: Boolean);
var
  Msg: TMsg;
  Accel: HACCEL;
  Keys: array[0..10] of TAccel;
  I: Integer;

  procedure Key(I: Integer; Virt: Byte; Code: Word; Cmd: Word);
  begin
    Keys[I].fVirt := Virt or FVIRTKEY;
    Keys[I].key := Code;
    Keys[I].cmd := Cmd;
  end;

begin
  Key(0, FCONTROL, Ord('O'), IDM_OPEN);
  for I := 0 to Ord(High(TRenderMode)) do
    Key(1 + I, 0, Ord('1') + I, IDM_MODE + I);
  Key(7, 0, Ord('F'), IDM_FIT);
  Key(8, 0, Ord('O'), IDM_ORTHO);
  Key(9, 0, Ord('M'), IDM_MEASURE);
  Key(10, 0, VK_ESCAPE, IDM_CLEAR);
  Accel := CreateAcceleratorTable(Keys[0], Length(Keys));
  try
    Viewer := TViewer.Create(ForceWarp);
  except
    on E: Exception do
    begin
      MessageBox(0, PChar(E.Message), 'DwgViewer', MB_ICONERROR);
      DestroyAcceleratorTable(Accel);
      Exit;
    end;
  end;
  try
    Viewer.Layout;
    ShowWindow(Viewer.FMain, SW_SHOW);
    if FileName <> '' then
      Viewer.Open(FileName);
    while GetMessage(Msg, 0, 0, 0) do
    begin
      // the wheel goes to the focused window; send it to the 3D view when
      // the cursor is over it
      if (Msg.message = WM_MOUSEWHEEL) and (WindowFromPoint(Msg.pt) = Viewer.FView) then
        Msg.hwnd := Viewer.FView;
      if TranslateAccelerator(Viewer.FMain, Accel, Msg) = 0 then
      begin
        TranslateMessage(Msg);
        DispatchMessage(Msg);
      end;
    end;
  finally
    FreeAndNil(Viewer);
    DestroyAcceleratorTable(Accel);
  end;
end;

end.
