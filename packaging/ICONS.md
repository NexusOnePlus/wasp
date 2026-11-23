# Organization icon

The client identity is the Wasp mark: three folded black and white panels on a
lime tile. The source is the maintainer-supplied `icon.png` (512 × 512, RGBA).

`linux/io.github.NexusOnePlus.Wasp.png` is the canonical image. It embeds the
Linux window icon and is copied into the user's icon search directory by the
desktop installer. `packaging/windows/wasp.png` is the same mark for Windows
notifications and the window; `packaging/windows/wasp.ico` carries native 16,
24, 32, 48, 64, 128 and 256 pixel frames.

The web uses an ICO with 16, 32 and 48 pixel frames, a 32 pixel PNG, and an
opaque 180 pixel Apple touch icon. These are generated from the same mark, with
its empty margins reduced for legibility. The Apple icon uses the page's
initial background colour. There is no traced or recoloured logo.

To regenerate, use Python 3.11 or newer and install the pinned tools:

```sh
python3 -m venv .venv-icons
. .venv-icons/bin/activate
python -m pip install -r packaging/requirements.txt
cargo install --locked --version 10.2.1 oxipng
python packaging/generate-icons.py
# Replace the source when the project chooses a new identity:
python packaging/generate-icons.py path/to/organization.png
```

The generator checks decoded pixel equality after every PNG optimization.
Trunk hashes the favicon ICO and copies the PNGs beside `index.html`; their
relative links also work in Pages previews and relocatable web archives.
