using System.Numerics;
using System.Runtime.InteropServices;
using Microsoft.UI.Composition;
using Microsoft.UI.Composition.SystemBackdrops;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using WinRT;
using SystemComposition = Windows.UI.Composition;
using DesktopWindowTarget = Windows.UI.Composition.Desktop.DesktopWindowTarget;

namespace Sonago;

internal sealed class CompactBackdrop : WinUIEx.TransparentTintBackdrop
{
    private readonly nint hwnd;
    private readonly Border panel;
    private readonly BrushTarget brushTarget;
    private DesktopAcrylicController? controller;
    private XamlRoot? xamlRoot;
    private DesktopWindowTarget? desktopTarget;
    private SystemComposition.ContainerVisual? card;
    private SystemComposition.SpriteVisual? material;
    private SystemComposition.CompositionRoundedRectangleGeometry? geometry;
    private SystemComposition.CompositionSpriteShape? stroke;
    private Vector3 offset;

    internal CompactBackdrop(nint hwnd, Border panel)
    {
        this.hwnd = hwnd;
        this.panel = panel;
        brushTarget = new BrushTarget(SetBrush);
    }

    protected override void OnTargetConnected(ICompositionSupportsSystemBackdrop target, XamlRoot root)
    {
        base.OnTargetConnected(target, root);
        if (!DesktopAcrylicController.IsSupported())
            return;

        controller = new DesktopAcrylicController();
        controller.SetSystemBackdropConfiguration(GetDefaultSystemBackdropConfiguration(target, root));
        if (controller.AddSystemBackdropTarget(brushTarget))
        {
            xamlRoot = root;
            panel.SizeChanged += OnLayoutChanged;
            panel.ActualThemeChanged += OnLayoutChanged;
            root.Changed += OnLayoutChanged;
        }
        else
        {
            controller.Dispose();
            controller = null;
        }
    }

    protected override void OnTargetDisconnected(ICompositionSupportsSystemBackdrop target)
    {
        if (xamlRoot is not null)
            xamlRoot.Changed -= OnLayoutChanged;
        xamlRoot = null;
        panel.SizeChanged -= OnLayoutChanged;
        panel.ActualThemeChanged -= OnLayoutChanged;
        controller?.Dispose();
        controller = null;
        ReleaseVisual();
        base.OnTargetDisconnected(target);
    }

    internal void MoveTo(int x, int y)
    {
        offset = new Vector3(x, y, 0);
        if (card is not null)
            card.Offset = offset;
    }

    private void OnLayoutChanged(object sender, object args) => UpdateGeometry();

    private void UpdateGeometry()
    {
        if (card is null || geometry is null || stroke is null || panel.XamlRoot is not { } root)
            return;

        float scale = (float)root.RasterizationScale;
        card.Size = new Vector2((float)panel.ActualWidth, (float)panel.ActualHeight) * scale;
        float thickness = (float)panel.BorderThickness.Left * scale;
        float inset = thickness / 2;
        geometry.Offset = new Vector2(inset);
        geometry.Size = Vector2.Max(Vector2.Zero, card.Size - new Vector2(thickness));
        float radius = Math.Clamp((float)panel.CornerRadius.TopLeft * scale - inset,
            0, Math.Min(geometry.Size.X, geometry.Size.Y) / 2);
        geometry.CornerRadius = new Vector2(radius);
        stroke.StrokeThickness = thickness;
        if (panel.BorderBrush is SolidColorBrush brush && stroke.StrokeBrush is SystemComposition.CompositionColorBrush colorBrush)
        {
            var color = brush.Color;
            color.A = (byte)(color.A * brush.Opacity);
            colorBrush.Color = color;
        }
    }

    private void SetBrush(SystemComposition.CompositionBrush? brush)
    {
        if (brush is null)
        {
            ReleaseVisual();
            return;
        }

        try
        {
            var compositor = brush.Compositor;
            if (material?.Compositor != compositor)
            {
                ReleaseVisual();
                AttachVisual(compositor);
            }
            material!.Brush = brush;
            UpdateGeometry();
            panel.Opacity = 0;
        }
        catch (COMException)
        {
            ReleaseVisual();
        }
    }

    private void AttachVisual(SystemComposition.Compositor compositor)
    {
        desktopTarget = CreateDesktopTarget(compositor);
        card = compositor.CreateContainerVisual();
        card.BorderMode = SystemComposition.CompositionBorderMode.Soft;
        card.Offset = offset;

        // Keep the acrylic and its outline on one curve in the system compositor.
        geometry = compositor.CreateRoundedRectangleGeometry();
        material = compositor.CreateSpriteVisual();
        material.RelativeSizeAdjustment = Vector2.One;
        material.Clip = compositor.CreateGeometricClip(geometry);
        card.Children.InsertAtBottom(material);

        stroke = compositor.CreateSpriteShape(geometry);
        stroke.StrokeBrush = compositor.CreateColorBrush();
        var outline = compositor.CreateShapeVisual();
        outline.RelativeSizeAdjustment = Vector2.One;
        outline.Shapes.Add(stroke);
        card.Children.InsertAtTop(outline);
        desktopTarget.Root = card;
    }

    private DesktopWindowTarget CreateDesktopTarget(SystemComposition.Compositor compositor)
    {
        var interop = compositor.As<ICompositorDesktopInterop>();
        Marshal.ThrowExceptionForHR(interop.CreateDesktopWindowTarget(hwnd, false, out nint abi));
        try
        {
            return MarshalInspectable<DesktopWindowTarget>.FromAbi(abi);
        }
        finally
        {
            Marshal.Release(abi);
        }
    }

    private void ReleaseVisual()
    {
        desktopTarget?.Dispose();
        desktopTarget = null;
        card?.Dispose();
        card = null;
        material?.Dispose();
        material = null;
        stroke?.Dispose();
        stroke = null;
        geometry?.Dispose();
        geometry = null;
        panel.Opacity = 1;
    }

    private sealed class BrushTarget(Action<SystemComposition.CompositionBrush?> update) : ICompositionSupportsSystemBackdrop
    {
        private SystemComposition.CompositionBrush? brush;

        public SystemComposition.CompositionBrush? SystemBackdrop
        {
            get => brush;
            set
            {
                brush = value;
                update(value);
            }
        }
    }

    [ComImport]
    [Guid("29E691FA-4567-4DCA-B319-D0F207EB6807")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface ICompositorDesktopInterop
    {
        [PreserveSig]
        int CreateDesktopWindowTarget(nint window, [MarshalAs(UnmanagedType.Bool)] bool isTopmost, out nint target);
    }
}
