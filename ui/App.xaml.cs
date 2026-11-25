using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;

namespace Hearlier;

public sealed partial class App : Application
{
    private readonly Native.Wake wake;
    private DispatcherQueue? dispatcher;
    private PanelWindow? panel;
    private TrayMenuWindow? trayMenu;
    private int pumpQueued;
    private volatile bool stopping;
    private bool started;

    public App()
    {
        InitializeComponent();
        wake = WakeDispatcher;
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        dispatcher = DispatcherQueue.GetForCurrentThread();
        try
        {
            bool? first = Native.Start(wake);
            if (first is null)
            {
                Shutdown();
                return;
            }
            started = true;
            if (first.Value)
            {
                Panel.ShowSetup();
            }
        }
        catch (Exception error)
        {
            stopping = true;
            Native.ShowStartupError(error.Message);
            Shutdown();
        }
    }

    private PanelWindow Panel => panel ??= new PanelWindow(Shutdown);

    private void WakeDispatcher()
    {
        if (stopping || Interlocked.Exchange(ref pumpQueued, 1) != 0)
        {
            return;
        }
        if (dispatcher?.TryEnqueue(Pump) != true)
        {
            Interlocked.Exchange(ref pumpQueued, 0);
        }
    }

    private void Pump()
    {
        Interlocked.Exchange(ref pumpQueued, 0);
        if (!started || stopping)
        {
            return;
        }
        foreach (string action in Native.Call<string[]>(new { action = "drain" }))
        {
            switch (action)
            {
                case "toggle":
                case "toggle_activate":
                    trayMenu?.Dismiss();
                    Panel.ToggleFromTray(activate: action == "toggle_activate");
                    break;
                case "outside_click":
                    panel?.DismissPassivePanel();
                    break;
                case "context_menu":
                    panel?.DismissPassivePanel();
                    trayMenu ??= new TrayMenuWindow(
                        settings => Panel.ShowFromTray(settings),
                        () => Native.Run(new { action = "quit" }));
                    trayMenu.ShowAtTray();
                    break;
                case "quit":
                    Shutdown();
                    return;
            }
        }
        panel?.Refresh();
    }

    private void Shutdown()
    {
        stopping = true;
        panel?.CloseForExit();
        if (started)
        {
            Native.Stop();
            started = false;
        }
        trayMenu?.CloseForExit();
        Exit();
    }
}
