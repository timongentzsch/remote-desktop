#!/bin/sh
# The whole regression: builds and checks the host, then runs it on every kind of desktop, each in a container.
# It prints every step's figures for reading; nothing but the checks at the start says pass or fail by itself.
# Needs Docker, the images (./images.sh) and /dev/uinput. Steps on the GPU's encoder run where Docker has an
# NVIDIA card to give.
cd "$(dirname "$0")"
docker info 2>/dev/null | grep -q "Runtimes:.*nvidia" && gpu="--gpus all -e NVIDIA_DRIVER_CAPABILITIES=all"
run() { timeout 200 docker run --rm --cap-add SYS_NICE --device /dev/uinput -v "$PWD":/test "$@" 2>&1; }
keep="frames (\|used\|encoding\|ended\|refused\|could not\|,yuv\|first video\|clipboard text\|sound packets\|after [0-9]* sessions\|host exited\|pasted\|sizes seen\|pointer\|shapes\|dB\|Hz\|left:\|right:"
screens="^screens\|picture\|asked\|no screen\|pointer\|first frame"

echo "== checks"
docker run --rm $gpu -v "$PWD/..":/src -v rd-test-target:/src/native/target -v rd-test-cargo:/usr/local/cargo/registry \
  -w /src/native rd-test-build sh -c '
    cargo fmt --check >/dev/null 2>&1 && echo formatted || echo "NOT FORMATTED"
    cargo clippy --release --all-targets -- --deny warnings >/dev/null 2>&1 && echo "clippy clean" || echo "CLIPPY FINDINGS"
    cargo test --release 2>&1 | grep "test result\|panicked"
    cargo build --release 2>&1 | grep -E "^(warning|error)" -A8
    install -o '"$(id -u) -g $(id -g)"' target/release/remote-desktop /src/tests/remote-desktop' || exit 1

echo "== sway: bursts";                 run rd-test-sway ./burst-test.sh 3000 10 "" | grep "$keep"
echo "== sway: motion, sharp colour";   run rd-test-sway ./motion-test.sh 3000 8 '"chroma":"444",' | grep "$keep"
echo "== sway: sound";                  run rd-test-sway ./sound-test.sh | grep "$keep"
echo "== sway: clipboard";              run rd-test-sway ./clipboard-test.sh sway | grep "$keep"
echo "== sway: still picture";          run rd-test-sway ./quality-test.sh 3000 10 "" | grep "PSNR\|frames,"
echo "== sway: two screens";            run rd-test-sway ./screens-test.sh sway | grep "$screens"
echo "== sway: sessions in a row";      run rd-test-sway ./sessions-test.sh 20 "" 0.6 | grep "host exited\|encoding" | sort | uniq -c
echo "== sway: two viewers";            run rd-test-sway ./two-viewers.sh | grep "$keep"
echo "== older sway: motion";           run rd-test-oldsway ./motion-test.sh 3000 8 "" | grep "$keep"
echo "== older sway: bursts";           run rd-test-oldsway ./burst-test.sh 3000 8 '"cursor":"yes",' | grep "$keep"
echo "== older sway: clipboard";        run rd-test-oldsway ./clipboard-test.sh sway | grep "$keep"
echo "== older sway: two screens";      run rd-test-oldsway ./screens-test.sh sway | grep "$screens"
echo "== install without systemd";      run rd-test-oldsway ./install-test.sh | grep "^installed\|^running\|^page\|^after"
echo "== GNOME: motion";                run rd-test-gnome ./gnome-test.sh 8 "" motion | grep "$keep"
echo "== GNOME: clipboard";             run rd-test-gnome ./gnome-clipboard.sh | grep "$keep"
echo "== GNOME: two screens";           run rd-test-gnome ./screens-test.sh mutter | grep "$screens"
echo "== GNOME: pointer shapes";        run rd-test-gnome ./pointer-test.sh mutter | grep "pointer\|shapes"
echo "== KDE: motion";                  run --device /dev/dri rd-test-kde ./kde-test.sh 8 "" motion | grep "$keep"
echo "== KDE: clipboard";               run --device /dev/dri rd-test-kde ./clipboard-test.sh kwin | grep "$keep"
echo "== KDE: two screens";             run --device /dev/dri rd-test-kde ./screens-test.sh kwin | grep "$screens"
echo "== X11: motion";                  run rd-test-x11 ./x11-test.sh 8 "" motion | grep "$keep"
echo "== X11: pointer, sessions";       run rd-test-x11 ./x11-extra.sh | grep "$keep"
echo "== X11: clipboard";               run rd-test-x11 ./x11-clipboard.sh | grep "$keep"
echo "== X11: resize";                  run rd-test-x11 ./x11-resize.sh | grep "$keep"
echo "== X11: three monitors";          run rd-test-x11 ./screens-test.sh x11 | grep "$screens"
echo "== X11: pointer shapes";          run rd-test-x11 ./pointer-test.sh x11 | grep "pointer\|shapes"
if [ -n "$gpu" ]; then
  echo "== GPU, sway: bursts";               run $gpu rd-test-sway ./burst-test.sh 3000 10 "" | grep "$keep"
  echo "== GPU, sway: motion, sharp colour"; run $gpu rd-test-sway ./motion-test.sh 3000 8 '"chroma":"444",' | grep "$keep"
  echo "== GPU, sway: still picture";        run $gpu rd-test-sway ./quality-test.sh 3000 10 "" | grep "PSNR\|frames,"
  echo "== GPU, X11: motion";                run $gpu rd-test-x11 ./x11-test.sh 8 "" motion | grep "$keep"
  echo "== GPU: 60 overlapping sessions";    run $gpu rd-test-sway ./sessions-test.sh 60 "" 0.6 | grep "host exited\|encoding" | sort | uniq -c
  echo "== GPU: two viewers";                run $gpu rd-test-sway ./two-viewers.sh | grep "$keep"
fi
echo "== done"
