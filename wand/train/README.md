# Training the spell recogniser

Pipeline, end to end:

```
capture.rs  ->  data/*.txt  ->  train_spells.py  ->  model/spells.tflite  ->  firmware
 (ESP32)        (recordings)     (Colab/laptop)       (int8 CNN)             (MicroFlow)
```

## 1. Record gestures

Flash the capture firmware and record one file per spell:

Run these from `wand/`, not from `wand/train/`:

```powershell
cargo build --release --bin capture
espflash flash --port COM6 target/xtensa-esp32s3-none-elf/release/capture
espflash monitor --port COM6 | Tee-Object data\left.txt
```

Press **BOOT**, wait for `# GO`, perform the gesture. Each press records 150
samples - 1.5 s at 100 Hz - of accelerometer + gyroscope data. Stop with Ctrl-C
when the file has enough repetitions, then start the next spell.

The model is trained on a **0.9 s window**, not the whole 1.5 s. The recording
is longer on purpose: a fast swipe is over in two or three hundred milliseconds,
which is quicker than anyone can time against a cue, so the extra length absorbs
your reaction time and the training script crops the busiest 0.9 s out of each
recording. Perform the gesture whenever you like after `# GO` - just make sure
it is finished before the recording ends.

The sensor runs at **±8 g** and **±1000 °/s**, not its defaults. At the default
±2 g, gravity has already spent one of those two, and a hard swipe flat-tops
against the other. A clipped reading looks the same whichever way the wand was
moving, so the defaults destroy exactly the part of a fast gesture that says
which one it was. Capture prints a warning if you still manage to run out of
range; the occasional clipped sample costs nothing, a lot of them means every
hard swing looks alike.

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

## How to perform the gestures

Inconsistent recordings are the usual reason these models perform badly, and
for short fast gestures the margin is thinner still - there are simply fewer
samples carrying the difference between one spell and another. These habits are
worth more than any amount of extra training.

**One stroke, then stop dead. Do not return to where you started.** This is the
big one. A return stroke is the mirror image of the outbound one, so "left" done
as out-and-back contains an acceleration left followed by an acceleration right -
which is exactly what "right" done as out-and-back contains, in the other order.
The model can learn to tell those apart, but it is spending its capacity on
ordering rather than on direction, and the moment the sliding window catches
only half the movement the two become genuinely identical. Swipe once,
decisively, and let the wand stop where it ends up.

**Bring it back slowly, after a pause.** Hold still for a moment once the stroke
finishes, then move the wand back gently. On-device there is a cooldown of about
a second after a spell fires, and a slow return stays under the movement
threshold, so neither the pause nor the return is ever mistaken for a gesture.
Recording it this way and casting it this way is the same motion, which is the
point.

**Start from still.** Half a second of stillness before the stroke gives the
window a clean lead-in, and matches what the wand sees in normal use.

**Fast, but not into the end stops.** Swing hard enough that the acceleration
peak is unmistakable. If capture starts warning about samples out of range on
most recordings, ease off slightly - a flat-topped peak carries no direction.

**Hold the wand the same way every time.** The model sees three axes, not "left"
in the room. Same face up, same end forward, same grip. If you rotate the wand
90° between recording and casting, every gesture becomes a different one.

**Record the way you will cast.** The most common failure is careful, deliberate
recordings followed by a quick flick during the demonstration. If you are going
to cast fast and loose, record fast and loose.

**Vary within a spell, not between.** Slightly different speeds and amplitudes
across repetitions make the model robust. A different *shape* of movement makes
it confused.

Aim for **40-50 repetitions per spell** - short gestures need more examples than
long ones, not fewer. `negative.txt` wants at least as many as any two spells
combined, and should include: the wand sitting still, held while you talk with
your hands, picked up and put down, carried across the room, and - importantly -
the slow return strokes and the near-misses that are almost a spell but not
quite. That last category is what stops it firing while you are explaining it.

## Recordings from the old format

Anything recorded before this change is 128 rows at 50 Hz with the sensor's
default ranges. Neither the length, the sample rate, nor the scaling matches,
and the gestures in them were performed slowly with a return to origin. The
training script detects them and says so rather than mis-parsing them. They
cannot be converted - re-record.

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
- `--augment 0` - disable augmentation. It normally adds three extra copies of
  the training set, each shifted in time, rescaled slightly, and given a little
  noise, so the model does not depend on a gesture sitting at one exact place in
  the window or being swung at one exact strength.
- `--epochs N` - defaults to 60, with early stopping on validation accuracy.
- `--negative-weight` - defaults to 1.0. The classes are balanced by weight
  before training, so `negative.txt` holding twice as many recordings as any
  spell does not teach the model that "not a spell" is twice as likely to be
  right. This scales the negative class on top of that balance: below 1.0 the
  model is readier to call a movement a spell. Try **0.7** if real gestures are
  being swallowed by negative.

### When one gesture keeps losing to negative

Check the confusion matrix first - it says whether the gesture is being called
negative, or being confused with another spell. Those need different fixes.

If it is losing to negative, in order of what to try:

1. `--negative-weight 0.7` and retrain. No re-recording.
2. Look at what is actually in `negative.txt`. A rejection class recorded from
   motions that resemble a spell will swallow that spell. **Picking the wand up
   off the desk is an upward movement**, so putting that in negative directly
   competes with the *up* spell - the two are asking the model to call the same
   motion two different things.
3. Re-record that spell more distinctly. For *up*, a sharp flick upward that
   stops dead is nothing like lifting the wand off a table; a gentle raise is
   exactly like it.

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
