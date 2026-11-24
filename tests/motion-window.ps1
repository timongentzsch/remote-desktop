# An interactive Windows fixture for interaction-check.mjs. Pass a stop-file path to close both windows.
param([Parameter(Mandatory=$true)][string]$StopFile)
$ErrorActionPreference = 'Stop'
Add-Type -ReferencedAssemblies System.Windows.Forms,System.Drawing -TypeDefinition @'
using System;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Windows.Forms;

public class MotionDesktop : Form {
    [DllImport("user32.dll")] static extern bool SetProcessDPIAware();
    readonly Form dialog = new Form();
    readonly Bitmap document;
    readonly Timer timer = new Timer();
    readonly Point previousPointer = Cursor.Position;
    int offset = 0;
    bool flash;

    MotionDesktop(string stopFile) {
        FormBorderStyle = FormBorderStyle.None;
        StartPosition = FormStartPosition.Manual;
        Bounds = Screen.PrimaryScreen.Bounds;
        DoubleBuffered = true;
        KeyPreview = true;
        document = new Bitmap(Width, Height * 2);
        string[] rows = {
            "    let viewport = window.innerHeight;     // visible rows in the document",
            "    if (position < viewport) return selection.start;",
            "    const coordinates = transform.map(point => point.offset);",
            "    function renderFrame(timestamp, previous) {"
        };
        using (var g = Graphics.FromImage(document))
        using (var font = new Font(FontFamily.GenericMonospace, 12, FontStyle.Regular, GraphicsUnit.Pixel)) {
            g.Clear(Color.FromArgb(28,36,47));
            for (int y = 0, i = 0; y < document.Height; y += 18, i++) {
                for (int x = 0; x < Width; x += 550)
                    g.DrawString((i + x).ToString("0000") + rows[(i + x) % rows.Length], font,
                                 i % 3 == 0 ? Brushes.LightBlue : Brushes.Wheat, x, y);
            }
        }
        dialog.FormBorderStyle = FormBorderStyle.FixedToolWindow;
        dialog.StartPosition = FormStartPosition.Manual;
        dialog.Size = new Size(Width / 4, Height / 4);
        dialog.BackColor = Color.AliceBlue;
        dialog.Text = "Project files";
        dialog.Paint += (sender, e) => {
            e.Graphics.FillRectangle(Brushes.Magenta, 0, 0, 12, dialog.ClientSize.Height);
            for (int i = 0; i * 23 < dialog.ClientSize.Height; i++) {
                int y = i * 23;
                e.Graphics.FillRectangle(Brushes.SteelBlue, 22, y + 6, 13, 10);
                e.Graphics.DrawString("Project document " + (i + 1) + ".txt", dialog.Font, Brushes.Black, 44, y + 4);
            }
        };
        dialog.KeyPreview = true;
        KeyEventHandler reset = (sender, e) => {
            if (e.KeyCode == Keys.Home) {
                offset = 0;
                PositionDialog();
                Invalidate();
            }
        };
        KeyDown += reset;
        dialog.KeyDown += reset;
        Shown += (sender, e) => {
            dialog.Show(this);
            PositionDialog();
        };
        MouseWheel += (sender, e) => {
            offset = ((offset - e.Delta) % document.Height + document.Height) % document.Height;
            Invalidate();
        };
        MouseDown += (sender, e) => { flash = !flash; Invalidate(); };
        timer.Interval = 200;
        timer.Tick += (sender, e) => { if (System.IO.File.Exists(stopFile)) Close(); };
        timer.Start();
    }

    void PositionDialog() {
        int caption = dialog.PointToScreen(Point.Empty).Y - dialog.Top;
        dialog.Location = new Point(Left + Width / 10, Top + (int)(Height * .115) - caption / 2);
    }

    protected override void OnPaint(PaintEventArgs e) {
        e.Graphics.DrawImageUnscaled(document, 0, -offset);
        if (offset + Height > document.Height)
            e.Graphics.DrawImageUnscaled(document, 0, document.Height - offset);
        e.Graphics.FillRectangle(Brushes.Lime, 28 + offset % (Width - 90), Height / 2, 30, Height * .03f);
        e.Graphics.FillRectangle(flash ? Brushes.White : Brushes.Black, Width * .89f, Height * .87f,
                                 Width * .10f, Height * .11f);
    }

    protected override void OnFormClosed(FormClosedEventArgs e) {
        timer.Dispose(); dialog.Dispose(); document.Dispose();
        Cursor.Position = previousPointer;
        base.OnFormClosed(e);
    }

    public static void Run(string stopFile) {
        SetProcessDPIAware();
        Application.EnableVisualStyles();
        using (var form = new MotionDesktop(stopFile)) Application.Run(form);
    }
}
'@
[MotionDesktop]::Run($StopFile)
