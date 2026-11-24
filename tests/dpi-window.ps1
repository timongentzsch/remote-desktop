# Displays the primary monitor's effective DPI for client-size-check.mjs.
param([Parameter(Mandatory=$true)][string]$StopFile)
$ErrorActionPreference = 'Stop'
Add-Type -ReferencedAssemblies System.Windows.Forms,System.Drawing -TypeDefinition @'
using System;
using System.Drawing;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows.Forms;

public class DpiWindow : Form {
    [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] static extern uint GetDpiForWindow(IntPtr window);

    protected override bool ShowWithoutActivation { get { return true; } }

    protected override void OnPaint(PaintEventArgs e) {
        int dpi = (int)GetDpiForWindow(Handle);
        e.Graphics.PageUnit = GraphicsUnit.Pixel;
        e.Graphics.FillRectangle(Brushes.Magenta, 0, 0, dpi + 24, 24);
        e.Graphics.FillRectangle(Brushes.Lime, 12, 0, dpi, 24);
    }

    public static void Run(string stopFile) {
        IntPtr previous = SetThreadDpiAwarenessContext(new IntPtr(-4)); // per-monitor aware v2
        if (previous == IntPtr.Zero) throw new Exception("Cannot enable per-monitor DPI awareness");
        try {
            using (var form = new DpiWindow())
            using (var timer = new Timer()) {
                form.FormBorderStyle = FormBorderStyle.None;
                form.AutoScaleMode = AutoScaleMode.None;
                form.StartPosition = FormStartPosition.Manual;
                form.ShowInTaskbar = false;
                form.TopMost = true;
                form.BackColor = Color.Black;
                var bounds = new Rectangle(24, 24, 520, 24);
                form.Bounds = bounds;
                uint lastDpi = 0;
                timer.Interval = 100;
                timer.Tick += (sender, e) => {
                    if (File.Exists(stopFile)) { form.Close(); return; }
                    // The OS may suggest a new window location when the desktop scale changes.
                    form.Bounds = bounds;
                    uint dpi = GetDpiForWindow(form.Handle);
                    if (dpi != lastDpi) {
                        lastDpi = dpi;
                        Console.WriteLine("Monitor DPI {0}", dpi);
                        form.Invalidate();
                    }
                };
                timer.Start();
                Application.Run(form);
            }
        } finally { SetThreadDpiAwarenessContext(previous); }
    }
}
'@
[DpiWindow]::Run($StopFile)
