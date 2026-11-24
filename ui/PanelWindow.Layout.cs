using System.Diagnostics;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;
using Windows.Graphics;
using Windows.UI.ViewManagement;

namespace Hearlier;

public sealed partial class PanelWindow
{
    private const double EntranceOffsetDip = 48;
    private const double SlideMilliseconds = 180;
    private readonly UISettings uiSettings = new();
    private (PointInt32 RestingPosition, int StartY, int EndY)? slide;
    private long slideStart;
    private bool slideReachedEnd;

    private void Resize(bool newAnchor = false, bool slideIn = false)
    {
        var interruptedExit = slide is { } current && current.EndY != current.RestingPosition.Y
            ? AppWindow.Position
            : (PointInt32?)null;
        StopSlide();
        var outer = AppWindow.Size;
        var client = AppWindow.ClientSize;
        double currentScale = WindowDpi() / 96d;
        double frameWidth = Math.Max(0, outer.Width - client.Width) / currentScale;
        double frameHeight = Math.Max(0, outer.Height - client.Height) / currentScale;

        int width = setup ? SettingsWidth : PanelWidth;
        QuickPanel.Measure(new Size(Math.Max(1, width - frameWidth), double.PositiveInfinity));
        double contentHeight = setup ? SetupHeight : QuickPanel.DesiredSize.Height;
        var place = Native.Call<Placement>(new
        {
            action = "place",
            width,
            height = (int)Math.Ceiling(contentHeight + frameHeight),
            centered = setup,
            newAnchor
        });

        var bounds = new RectInt32(place.X, place.Y, place.Width, place.Height);
        if (slideIn && uiSettings.AnimationsEnabled)
        {
            var restingPosition = new PointInt32(bounds.X, bounds.Y);
            if (interruptedExit is { } previous && previous.X == bounds.X)
                bounds.Y = Math.Max(bounds.Y, previous.Y);
            else
                bounds.Y += Math.Min(bounds.Height, (int)Math.Round(EntranceOffsetDip * place.Dpi / 96d));
            if (bounds.Y != restingPosition.Y)
                StartSlide(restingPosition, bounds.Y, restingPosition.Y);
        }
        var position = AppWindow.Position;
        if (position.X != bounds.X || position.Y != bounds.Y || outer.Width != bounds.Width || outer.Height != bounds.Height)
        {
            AppWindow.MoveAndResize(bounds);
        }
    }

    private void SlideOut()
    {
        var position = AppWindow.Position;
        var restingPosition = slide?.RestingPosition ?? position;
        StopSlide();
        if (!setup && uiSettings.AnimationsEnabled)
        {
            var work = DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Nearest).WorkArea;
            int bottom = work.Y + work.Height;
            if (position.Y < bottom)
            {
                StartSlide(restingPosition, position.Y, bottom);
                return;
            }
        }
        AppWindow.Hide();
    }

    private void StartSlide(PointInt32 restingPosition, int startY, int endY)
    {
        slide = (restingPosition, startY, endY);
        slideStart = Stopwatch.GetTimestamp();
        slideReachedEnd = false;
        CompositionTarget.Rendering += AdvanceSlide;
    }

    private void AdvanceSlide(object? sender, object args)
    {
        if (slide is not { } current)
            return;

        double progress = Stopwatch.GetElapsedTime(slideStart).TotalMilliseconds / SlideMilliseconds;
        if (progress >= 1 && (shown || slideReachedEnd))
        {
            FinishSlide();
            return;
        }

        // Render the final offscreen position before hiding the window.
        slideReachedEnd = progress >= 1;
        progress = Math.Min(1, progress);
        double remaining = 1 - progress;
        double eased = shown ? 1 - remaining * remaining * remaining : progress * progress;
        int y = current.StartY + (int)Math.Round((current.EndY - current.StartY) * eased);
        if (AppWindow.Position.Y != y)
            AppWindow.Move(new PointInt32(current.RestingPosition.X, y));
    }

    private void FinishSlide()
    {
        if (slide is not { } current)
            return;

        StopSlide();
        if (shown)
            AppWindow.Move(current.RestingPosition);
        else
            AppWindow.Hide();
    }

    private void StopSlide()
    {
        if (slide is null)
            return;

        CompositionTarget.Rendering -= AdvanceSlide;
        slide = null;
    }

    private uint WindowDpi()
    {
        uint dpi = Native.GetDpiForWindow(WinRT.Interop.WindowNative.GetWindowHandle(this));
        return dpi == 0 ? 96 : dpi;
    }
}
