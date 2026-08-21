# Training the spell recogniser

Pipeline, end to end:

```
capture.rs  ->  data/*.txt  ->  train_spells.py  ->  model/spells.tflite  ->  firmware
 (ESP32)        (recordings)     (Colab/laptop)       (int8 CNN)             (MicroFlow)
```

## 1. Record gestures

Flash the capture firmware and record one file per spell:

Run these from the repository root, not from `train/`:

```powershell
cargo build --release --bin capture
espflash flash --port COM6 target/xtensa-esp32s3-none-elf/release/capture
espflash monitor --port COM6 | Tee-Object data\left.txt
```

Press **BOOT**, wait for `# GO`, perform the gesture. Each press records a
128-sample window (2.56 s at 50 Hz) of accelerometer + gyroscope data. Stop
with Ctrl-C when the file has enough repetitions, then start the next spell.

`Tee-Object` shows the output while saving it, so you can see `# GO` and
confirm each gesture was recorded - recording blind makes it easy to miss a
failed capture. Note that Windows PowerShell 5.1's `Tee-Object` has no
`-Encoding` parameter and writes UTF-16; the parser detects that automatically,
so no flag is needed.

Then check what actually landed in the files:

```powershell
python train\check_captures.py data
```

Record roughly 25 repetitions per spell into these files:

| File | Gesture | Router action |
|---|---|---|
| `left.txt` | sweep left | cycle to previous interface |
| `right.txt` | sweep right | cycle to next interface |
| `up.txt` | sweep up | turn on interface |
| `down.txt` | sweep down | shutdown interface |
| `push.txt` | thrust forward | stress test (iperf3 UDP) |
| `circular.txt` | draw a circle | backup router config |
| `negative.txt` | idle, fidgeting, setting it down | nothing |

`negative.txt` matters as much as the others: without it the model has no way
to say "that wasn't a spell" and will fire constantly. Record plenty, and make
it varied.

Keep each gesture consistent between repetitions - inconsistent recordings are
the usual reason these models perform badly.

## 2. Train

TensorFlow has no build for Python 3.14, so train either in **Google Colab**
(what the original MagicWand project does) or under a local Python 3.11-3.13.

In Colab: upload `train_spells.py` and the `data/` folder, then

```python
!python train_spells.py --data-dir data --out model
```

Locally, with a supported Python:

```
pip install tensorflow numpy
python train/train_spells.py --data-dir data --out model
```

Useful flags:

- `--channels 3` - accelerometer only, like the original MagicWand model.
  The default `6` adds the gyroscope, which helps most with *circular* and *push*.
- `--augment 0` - disable the time-shift/noise augmentation.
- `--epochs N` - defaults to 60, with early stopping on validation accuracy.

The script prints a confusion matrix and both float and quantised test
accuracy. If the quantised figure is much worse than the float one, the model
lost too much in int8 conversion and needs more or cleaner data.

## 3. Outputs

- `model/spells.tflite` - int8-quantised CNN
- `model/labels.txt` - class names, in the order the model outputs them

## Why this architecture

The layer choice is dictated by what MicroFlow can execute on the ESP32:
`Conv2D`, `DepthwiseConv2D`, `FullyConnected`, `AveragePool2D`, `Reshape`, and
ReLU/ReLU6/Softmax - all quantised. There is no `MaxPool2D`, which the original
MagicWand model uses, so this one uses average pooling instead. The model must
be fully int8-quantised for the same reason.
