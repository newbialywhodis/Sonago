using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Hosting;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.Windows.Storage.Pickers;
using VirtualKey = Windows.System.VirtualKey;

namespace Hearlier;

public sealed partial class PanelWindow : Window
{
    private const int PanelWidth = 368;
    private const int SettingsWidth = 512;
    private const int SettingsHeight = 598;
    private const int SetupHeight = 442;

    private readonly Action shutdown;
    private readonly DispatcherQueueTimer refreshTimer;
    private readonly DispatcherQueueTimer dismissTimer;
    private bool setup;
    private bool shown;
    private bool active;
    private bool pickingFolder;
    private bool exiting;
    private string? commandError;
    private string? settingsError;
    private Preferences? draft;
    private MenuFlyout? openMenu;

    internal PanelWindow(Action shutdown)
    {
        this.shutdown = shutdown;
        InitializeComponent();
        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "hearlier.ico"));
        var presenter = OverlappedPresenter.Create();
        presenter.SetBorderAndTitleBar(true, false);
        presenter.IsResizable = false;
        presenter.IsMaximizable = false;
        presenter.IsMinimizable = false;
        AppWindow.SetPresenter(presenter);
        AppWindow.Closing += OnClosing;
        Activated += OnActivated;

        var escape = new KeyboardAccelerator { Key = VirtualKey.Escape };
        escape.Invoked += OnEscape;
        Root.KeyboardAccelerators.Add(escape);
        Root.Loaded += (_, _) => PresentSettingsPopup();
        foreach (var (button, menu) in new (SplitButton, MenuFlyout)[]
                 {
                     (SaveButton, SaveMenu), (Microphone, MicrophoneMenu), (SystemAudio, SystemMenu)
                 })
        {
            menu.SystemBackdrop = new DesktopAcrylicBackdrop();
            menu.Opened += (_, _) => SetChevronRotation(button, 180);
            menu.Closed += (_, _) => SetChevronRotation(button, 0);
        }

        refreshTimer = DispatcherQueue.CreateTimer();
        refreshTimer.Interval = TimeSpan.FromSeconds(1);
        refreshTimer.Tick += (_, _) => Refresh();
        dismissTimer = DispatcherQueue.CreateTimer();
        dismissTimer.Interval = TimeSpan.FromMilliseconds(180);
        dismissTimer.IsRepeating = false;
        dismissTimer.Tick += (_, _) => DismissIfInactive();
    }

    internal void ShowSetup()
    {
        setup = true;
        OpenSettings();
        Show(newAnchor: true);
    }

    internal void ToggleFromTray()
    {
        if (shown && !setup && !pickingFolder)
        {
            Hide();
        }
        else if (!pickingFolder)
        {
            ShowFromTray(settings: false);
        }
    }

    internal void ShowFromTray(bool settings)
    {
        if (pickingFolder)
        {
            return;
        }
        if (settings && draft is null)
        {
            OpenSettings();
        }
        Show(newAnchor: !shown);
    }

    private void Show(bool newAnchor)
    {
        dismissTimer.Stop();
        AppWindow.IsShownInSwitchers = setup;
        QuickPanel.Visibility = setup ? Visibility.Collapsed : Visibility.Visible;
        Title = setup ? "Set up hearlier" : "hearlier";
        shown = true;
        Refresh(resize: false);
        Resize(newAnchor, slideIn: newAnchor && !setup && draft is null);
        Activate();
        refreshTimer.Start();
        DispatcherQueue.TryEnqueue(() =>
        {
            if (shown)
            {
                if (setup)
                    SystemEnabled.Focus(FocusState.Programmatic);
                else if (draft is not null)
                    PresentSettingsPopup();
                else
                    Gear.Focus(FocusState.Programmatic);
            }
        });
    }

    private void Hide()
    {
        if (!shown)
        {
            return;
        }
        shown = false;
        dismissTimer.Stop();
        refreshTimer.Stop();
        openMenu?.Hide();
        CloseSettings(restoreFocus: false);
        commandError = null;
        SlideOut();
    }

    internal void CloseForExit()
    {
        exiting = true;
        shown = false;
        refreshTimer.Stop();
        dismissTimer.Stop();
        StopSlide();
        Close();
    }

    internal void Refresh() => Refresh(resize: true);

    private void Refresh(bool resize)
    {
        if (!shown)
        {
            return;
        }
        var state = Native.Call<Snapshot>(new { action = "snapshot" });
        Microphone.IsEnabled = SystemAudio.IsEnabled = draft is null;
        Microphone.IsChecked = state.MicOn;
        MicrophoneIcon.Glyph = state.MicOn ? "\uE720" : "\uEC54";
        SystemAudio.IsChecked = state.SystemOn;
        SystemAudioIcon.Glyph = state.SystemOn ? "\uE767" : "\uE74F";
        SaveButton.IsEnabled = draft is null && state.CanSave;
        ToolTipService.SetToolTip(SaveButton, state.SaveLabel);
        AutomationProperties.SetHelpText(SaveButton, state.SaveLabel);
        string warning = commandError ?? state.Warning ?? "";
        bool needsResize = PanelWarning.Text != warning;
        PanelWarning.Text = warning;
        PanelWarning.Visibility = warning.Length == 0 ? Visibility.Collapsed : Visibility.Visible;
        UpdateSettingsNotice(settingsError ?? draft?.Issue ?? state.Issue);
        if (resize && needsResize && !setup)
        {
            Resize();
        }
    }

    private void OnClosing(AppWindow sender, AppWindowClosingEventArgs args)
    {
        if (exiting)
        {
            return;
        }
        args.Cancel = true;
        if (!pickingFolder)
        {
            DispatcherQueue.TryEnqueue(Dismiss);
        }
    }

    private void OnActivated(object sender, WindowActivatedEventArgs e)
    {
        active = e.WindowActivationState != WindowActivationState.Deactivated;
        if (active)
        {
            dismissTimer.Stop();
        }
        else if (shown && !setup && !pickingFolder)
        {
            // Give the tray callback time to handle the same click that moved activation.
            dismissTimer.Start();
        }
    }

    private void DismissIfInactive()
    {
        if (shown && !active && !setup && !pickingFolder && openMenu is null)
        {
            if (Native.Call<bool>(new { action = "tray_pointer_down" }))
                dismissTimer.Start();
            else
                Hide();
        }
    }

    private void Dismiss()
    {
        if (setup)
            Quit(this, new RoutedEventArgs());
        else
            Hide();
    }

    private void OnEscape(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        if (!shown || pickingFolder)
        {
            return;
        }
        args.Handled = true;
        if (openMenu is not null)
            openMenu.Hide();
        else if (!setup && draft is not null)
            CloseSettings();
        else
            Dismiss();
    }

    private void ToggleMicrophone(SplitButton sender, SplitButtonClickEventArgs args) =>
        QuickCommand(new { action = "toggle_source", track = 1 });

    private void ToggleSystem(SplitButton sender, SplitButtonClickEventArgs args) =>
        QuickCommand(new { action = "toggle_source", track = 0 });

    private void Save(SplitButton sender, SplitButtonClickEventArgs args) =>
        QuickCommand(new { action = "save" });

    private void OpenMicrophoneMenu(object sender, object args) => OpenDeviceMenu(MicrophoneMenu, 1);
    private void OpenSystemMenu(object sender, object args) => OpenDeviceMenu(SystemMenu, 0);

    private void OpenDeviceMenu(MenuFlyout menu, int track)
    {
        openMenu = menu;
        menu.Items.Clear();
        try
        {
            foreach (var device in Native.Call<DeviceChoice[]>(new { action = "devices", track }))
            {
                var item = new RadioMenuFlyoutItem
                {
                    Text = device.Label,
                    GroupName = track == 0 ? "SystemOutput" : "MicrophoneInput",
                    IsChecked = device.Selected
                };
                item.Click += (_, _) => QuickCommand(new { action = "select_device", track, device.Endpoint });
                menu.Items.Add(item);
            }
        }
        catch (InvalidOperationException error)
        {
            menu.Items.Add(new MenuFlyoutItem { Text = "Could not list audio devices", IsEnabled = false });
            commandError = error.Message;
            Refresh();
        }
    }

    private void OpenSaveMenu(object sender, object args)
    {
        openMenu = SaveMenu;
        SaveMenu.Items.Clear();
        foreach (var length in Native.Call<SaveLength[]>(new { action = "save_lengths" }))
        {
            var item = new MenuFlyoutItem { Text = length.Label };
            if (length.IsDefault)
            {
                item.Icon = new SymbolIcon(Symbol.Accept);
            }
            item.Click += (_, _) => QuickCommand(new { action = "save", length.Minutes });
            SaveMenu.Items.Add(item);
        }
    }

    private void CloseMenu(object sender, object args)
    {
        if (ReferenceEquals(openMenu, sender))
        {
            openMenu = null;
        }
        DispatcherQueue.TryEnqueue(DismissIfInactive);
    }

    private void SetChevronRotation(SplitButton button, float degrees)
    {
        // Reuse the chevron from the WinUI split button template.
        if (VisualTreeHelper.GetChildrenCount(button) == 0 ||
            VisualTreeHelper.GetChild(button, 0) is not Grid root ||
            root.Children.OfType<Button>().FirstOrDefault(part => part.Name == "SecondaryButton")
                is not { Content: FrameworkElement chevron })
            return;

        var visual = ElementCompositionPreview.GetElementVisual(chevron);
        const string rotation = nameof(visual.RotationAngleInDegrees);
        visual.CenterPoint = new((float)(chevron.ActualWidth / 2), (float)(chevron.ActualHeight / 2), 0);
        if (uiSettings.AnimationsEnabled)
        {
            var animation = visual.Compositor.CreateScalarKeyFrameAnimation();
            animation.Duration = TimeSpan.FromMilliseconds(180);
            animation.InsertKeyFrame(1, degrees);
            visual.StartAnimation(rotation, animation);
        }
        else
        {
            visual.StopAnimation(rotation);
            visual.RotationAngleInDegrees = degrees;
        }
    }

    private void QuickCommand(object command)
    {
        if (!shown)
        {
            return;
        }
        commandError = null;
        try
        {
            Native.Run(command);
        }
        catch (InvalidOperationException error)
        {
            commandError = error.Message;
        }
        Refresh();
    }

    private void ToggleSettings(object sender, RoutedEventArgs args)
    {
        if (!shown)
        {
            return;
        }
        if (draft is not null)
        {
            CloseSettings();
        }
        else
        {
            OpenSettings();
            Refresh();
            PresentSettingsPopup();
        }
    }

    private void PresentSettingsPopup()
    {
        if (!shown || setup || draft is null || !Root.IsLoaded || SettingsPopup.IsOpen)
            return;

        FinishSlide();
        if (SettingsPopup.Child is null)
        {
            Root.Children.Remove(SettingsPanel);
            SettingsPanel.Visibility = Visibility.Visible;
            SettingsPopup.Child = SettingsPanel;
        }

        var work = DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Primary).WorkArea;
        double scale = Root.XamlRoot.RasterizationScale;
        SettingsPanel.Width = Math.Clamp(work.Width / scale - 24, 1, SettingsWidth);
        // Leave room for screen-edge padding at the top of the work area.
        SettingsPanel.Height = Math.Clamp((AppWindow.Position.Y - work.Y) / scale - 16, 1, SettingsHeight);
        SettingsPopup.IsOpen = true;
        SystemEnabled.Focus(FocusState.Programmatic);
    }

    private void OpenSettings()
    {
        draft = Native.Call<Preferences>(new { action = "settings" });
        var settings = draft.Settings;
        SettingsTitle.Text = setup ? "Set up hearlier" : "Settings";
        SetupDescription.Visibility = setup ? Visibility.Visible : Visibility.Collapsed;
        FormatRow.Visibility = FolderRow.Visibility = General.Visibility = QuitButton.Visibility =
            setup ? Visibility.Collapsed : Visibility.Visible;
        ApplyButton.Content = setup ? "Start capturing" : "Apply";
        CancelButton.Content = setup ? "Exit" : "Cancel";
        SystemEnabled.IsChecked = settings.Sources[0].Enabled;
        MicEnabled.IsChecked = settings.Sources[1].Enabled;
        SystemDevice.ItemsSource = draft.Devices[0].Select(device => device.Label).ToArray();
        MicDevice.ItemsSource = draft.Devices[1].Select(device => device.Label).ToArray();
        SystemDevice.SelectedIndex = Array.FindIndex(draft.Devices[0], device => device.Selected);
        MicDevice.SelectedIndex = Array.FindIndex(draft.Devices[1], device => device.Selected);
        Retention.Value = settings.Retention;
        Minutes.Value = settings.Minutes;
        Hotkey.Text = settings.Hotkey;
        Folder.Text = settings.Output;
        Format.SelectedIndex = settings.Format == "ogg_opus" ? 1 : 0;
        Autostart.IsChecked = settings.Autostart;
        Notifications.IsChecked = settings.Notifications;
        if (setup)
            SettingsPanel.Visibility = Visibility.Visible;
        AutomationProperties.SetName(Gear, "Close settings");
        ToolTipService.SetToolTip(Gear, "Close settings");
        UpdateSettingsNotice(draft.Issue);
    }

    private void CloseSettings(bool restoreFocus = true)
    {
        if (draft is null || (restoreFocus && !shown))
        {
            return;
        }
        SettingsPopup.IsOpen = false;
        draft = null;
        settingsError = null;
        if (SettingsPopup.Child is null)
            SettingsPanel.Visibility = Visibility.Collapsed;
        AutomationProperties.SetName(Gear, "Settings");
        ToolTipService.SetToolTip(Gear, "Settings");
        if (restoreFocus)
        {
            Refresh();
            Gear.Focus(FocusState.Programmatic);
        }
    }

    private async void Browse(object sender, RoutedEventArgs args)
    {
        pickingFolder = true;
        try
        {
            var result = await new FolderPicker(AppWindow.Id).PickSingleFolderAsync();
            if (result is not null)
            {
                Folder.Text = result.Path;
                settingsError = null;
            }
        }
        catch (Exception error)
        {
            settingsError = error.Message;
        }
        finally
        {
            pickingFolder = false;
        }
        Refresh();
    }

    private void Apply(object sender, RoutedEventArgs args)
    {
        if (!shown || draft is null)
        {
            return;
        }
        var original = draft.Settings;
        try
        {
            var settings = original with
            {
                Sources =
                [
                    new Source(SystemEnabled.IsChecked == true, SelectedEndpoint(0, SystemDevice)),
                    new Source(MicEnabled.IsChecked == true, SelectedEndpoint(1, MicDevice))
                ],
                Retention = ReadMinutes(Retention, "history limit"),
                Minutes = ReadMinutes(Minutes, "default save length"),
                Output = Folder.Text == original.Output ? original.Output : Folder.Text.Trim(),
                Format = Format.SelectedIndex == 1 ? "ogg_opus" : "matroska",
                Hotkey = Hotkey.Text.Trim(),
                Autostart = Autostart.IsChecked == true,
                Notifications = Notifications.IsChecked == true
            };
            Native.Run(new { action = "apply", settings });
        }
        catch (InvalidOperationException error)
        {
            settingsError = error.Message;
            Refresh();
            return;
        }
        if (setup)
        {
            setup = false;
            Hide();
        }
        else
        {
            commandError = null;
            CloseSettings();
        }
    }

    private string? SelectedEndpoint(int track, ComboBox combo) =>
        combo.SelectedIndex >= 0 ? draft!.Devices[track][combo.SelectedIndex].Endpoint
            : draft!.Settings.Sources[track].Endpoint;

    private static int ReadMinutes(NumberBox box, string label)
    {
        double value = box.Value;
        if (!(value is >= 1 and <= 720) || Math.Truncate(value) != value)
        {
            throw new InvalidOperationException($"Enter a whole number from 1 to 720 for the {label}.");
        }
        return (int)value;
    }

    private void Cancel(object sender, RoutedEventArgs args) => DismissSettings();

    private void DismissSettings()
    {
        if (setup)
            Dismiss();
        else
            CloseSettings();
    }

    private void Quit(object sender, RoutedEventArgs args)
    {
        try
        {
            Native.Run(new { action = "can_quit" });
        }
        catch (InvalidOperationException error)
        {
            settingsError = error.Message;
            Refresh();
            return;
        }
        shutdown();
    }

    private void UpdateSettingsNotice(string? issue)
    {
        SettingsNotice.Message = issue ?? "";
        SettingsNotice.Severity = settingsError is null ? InfoBarSeverity.Warning : InfoBarSeverity.Error;
        SettingsNotice.IsOpen = draft is not null && issue is not null;
    }
}
