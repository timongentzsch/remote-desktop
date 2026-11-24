"""Scroll, drag and click fixture for interaction-check.mjs on an isolated Tk desktop."""
import argparse
import os
import re
import subprocess
import tkinter as tk
from pathlib import Path

args = argparse.ArgumentParser(description=__doc__)
args.add_argument("--geometry", help="screen size and signed desktop origin")
args.add_argument("--stop-file", type=Path)
options = args.parse_args()
root = tk.Tk()
root.title("Remote Desktop motion test")
if options.geometry:
    match = re.fullmatch(r"(\d+)x(\d+)([+-]\d+)([+-]\d+)", options.geometry)
    if not match:
        args.error("geometry must have the form 1280x800+0+0")
    width, height, left, top = map(int, match.groups())
else:
    width, height = root.winfo_screenwidth(), root.winfo_screenheight()
    left = top = 0
if root.tk.call("tk", "windowingsystem") == "x11":
    root.attributes("-fullscreen", True)
else:
    root.overrideredirect(True)
root.geometry(f"{width}x{height}+{left}+{top}")
page = tk.Canvas(root, bg="#1c242f", highlightthickness=0)
page.pack(fill="both", expand=True)
lines = ("let viewport = window.innerHeight;", "if (row < viewport) return selection.start;",
         "const coordinates = transform.map(point => point.offset);", "function renderFrame(timestamp, previous) {")
for row, y in enumerate(range(0, 3 * height + 18, 18)):
    text = f"{row:04}    {lines[row % len(lines)]:56} " * (width // 450 + 1)
    page.create_text(0, y, text=text, font=("Courier", -13), fill=("#add8e6", "#f5deb3")[row % 2],
                     anchor="nw", tags="document")
marker = page.create_rectangle(0, 0, 0, 0, fill="#00ff00", width=0)
flash = page.create_rectangle(width * .89, height * .87, width * .99, height * .98, fill="black", width=0)
offset = 0


def scroll(amount):
    global offset
    target = (offset + amount) % (2 * height)
    page.move("document", 0, offset - target)
    offset = target
    x = 28 + offset % (width - 90)
    page.coords(marker, x, height * .50, x + 30, height * .53)


page.bind("<MouseWheel>", lambda e: scroll(11 if e.delta < 0 else -11))
page.bind("<Button-4>", lambda _: scroll(-11))
page.bind("<Button-5>", lambda _: scroll(11))

def click(_):
    colour = "white" if page.itemcget(flash, "fill") == "black" else "black"
    page.itemconfigure(flash, fill=colour)


page.bind("<Button-1>", click)

dialog = tk.Toplevel(root)
dialog.title("Project files")
dialog.transient(root)
dialog.attributes("-topmost", True)
dialog.resizable(False, False)
folder = tk.Canvas(dialog, width=width // 4, height=height // 4, bg="#f0f8ff", highlightthickness=0)
folder.pack()
folder.create_rectangle(0, 0, 12, height // 4, fill="magenta", width=0)
for row, y in enumerate(range(15, height // 4, 23), 1):
    folder.create_rectangle(22, y - 5, 35, y + 5, fill="#4682b4", width=0)
    folder.create_text(44, y, text=f"Project document {row}.txt", anchor="w")

# Window managers differ in their title-bar heights. Place the real title bar beneath the
# browser's drag start (18%,11.5%) and the magenta content across its sample row (19%).
dialog_left, dialog_top = left + width // 10, top + height // 10

def position():
    if os.environ.get("SWAYSOCK"):
        # Sway controls the position of a floating XWayland window itself.
        subprocess.run(["swaymsg", "-q", f'[pid={os.getpid()} title="^Project files$"] move position {dialog_left} {dialog_top}'])
    else:
        dialog.geometry(f"+{dialog_left}+{dialog_top}")


def reset(_=None):
    scroll(-offset)
    position()
    dialog.lift()
    dialog.focus_force()


def align():
    global dialog_top
    dialog_top += top + round(.115 * height) + 9 - dialog.winfo_rooty()
    reset()


root.bind_all("<Home>", reset)
root.update_idletasks()
reset()
root.after(300, align)
if options.stop_file:
    def check_stop():
        if options.stop_file.exists():
            root.destroy()
        else:
            root.after(200, check_stop)
    root.after(200, check_stop)
root.mainloop()
