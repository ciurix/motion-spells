#!/usr/bin/env python3
"""Train the spell gesture recogniser and export it for the ESP32.

Reads the CSV windows produced by `src/bin/capture.rs`, trains a small CNN, and
writes an int8-quantised .tflite model that the MicroFlow crate compiles into
the firmware.

The architecture is constrained by what MicroFlow can execute on-device:
Conv2D, DepthwiseConv2D, FullyConnected, AveragePool2D, Reshape, plus
ReLU/ReLU6/Softmax - all quantised. Note there is no MaxPool2D, which is why
this uses average pooling where the original MagicWand model used max pooling.

Usage:
    python train/train_spells.py --data-dir data --out model
"""

import argparse
import pathlib
import sys

import numpy as np

# Samples the model sees: 90 at 100 Hz is a 0.9 s window. Must match WINDOW and
# PERIOD_MS in src/bin/spells.rs.
WINDOW = 90
# Samples per recording - must match CAPTURE in src/bin/capture.rs. Longer than
# WINDOW so nobody has to start a gesture on cue; the extra is cropped away.
CAPTURE = 150
# Raw counts -> physical units, at the ranges capture.rs configures
# (+-8 g and +-1000 deg/s, not the sensor's defaults).
ACCEL_LSB_PER_G = 4096.0
GYRO_LSB_PER_DPS = 32.8
# A reading this large ran out of range and its true value is unknown.
CLIP_LEVEL = 32_000

# The firmware's movement detector, mirrored here so that training windows are
# cut at the same instant the wand cuts them. These must match RECENT,
# ACTIVE_GATE and QUIET_GATE in src/bin/spells.rs.
RECENT = 12
ACTIVE_GATE = 2.0
QUIET_GATE = 0.8


def is_separator(line):
    """True only for the all-dashes marker line.

    Checking every field matters: sensor values are often negative, so a naive
    "starts with -" test would treat a normal data row as a separator.
    """
    parts = line.split(",")
    return len(parts) > 1 and all(p.strip() == "-" for p in parts)


def read_text_any_encoding(path):
    """Read a capture file whatever encoding the terminal wrote it in.

    PowerShell's `>` redirection produces UTF-16 by default, so decoding as
    UTF-8 would fail on a file that is otherwise perfectly good.
    """
    raw = path.read_bytes()
    for bom, enc in (
        (b"\xff\xfe", "utf-16-le"),
        (b"\xfe\xff", "utf-16-be"),
        (b"\xef\xbb\xbf", "utf-8-sig"),
    ):
        if raw.startswith(bom):
            return raw.decode(enc, errors="ignore")
    # Heuristic: lots of NUL bytes means UTF-16 written without a BOM.
    if raw.count(b"\x00") > len(raw) // 4:
        return raw.decode("utf-16-le", errors="ignore")
    return raw.decode("utf-8", errors="ignore")


def parse_capture_file(path):
    """Split one capture file into a list of (CAPTURE, 6) float arrays.

    The firmware prints a separator line before each gesture and comment lines
    starting with '#', so recordings are whatever sits between separators.
    """
    windows, current = [], []
    # Counted rather than reported one by one: a whole file in the wrong format
    # would otherwise print the same complaint a hundred times over.
    dropped_old, dropped_short = [], []

    def flush():
        if not current:
            return
        if len(current) == CAPTURE:
            windows.append(np.array(current, dtype=np.float32))
        elif len(current) == 128:
            # The old format: 128 samples at 50 Hz, taken at the sensor's
            # default ranges. Neither the length nor the scaling matches, and
            # the gestures were performed slowly, so these cannot be reused.
            dropped_old.append(len(current))
        else:
            dropped_short.append(len(current))

    for line in read_text_any_encoding(path).splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if is_separator(line):
            flush()
            current = []
            continue
        parts = line.split(",")
        if len(parts) != 6:
            continue
        try:
            current.append([float(p) for p in parts])
        except ValueError:
            continue  # partial line from a reset mid-print
    flush()

    if dropped_old:
        print(
            f"  ! {path.name}: {len(dropped_old)} recordings are in the old 128-row "
            f"50 Hz format and cannot be converted - re-record this file",
            file=sys.stderr,
        )
    if dropped_short:
        print(
            f"  ! {path.name}: dropped {len(dropped_short)} recordings of the wrong "
            f"length (expected {CAPTURE} rows, saw {sorted(set(dropped_short))}) - "
            f"interrupted part-way through",
            file=sys.stderr,
        )
    return windows


def runtime_activity(window):
    """The firmware's movement measure, evaluated at every sample.

    A copy of `recent_activity` in spells.rs: the mean change per channel over
    the last RECENT samples. Takes physical units, as the firmware does.
    """
    n, channels = window.shape
    span = RECENT - 1

    steps = np.zeros(n)
    steps[1:] = np.abs(np.diff(window, axis=0)).sum(axis=1)

    cumulative = np.concatenate([[0.0], np.cumsum(steps)])
    activity = np.zeros(n)
    if n > span:
        activity[span:] = (cumulative[span + 1:] - cumulative[1:n - span + 1]) / (span * channels)
    return activity


def crop_at_runtime_trigger(window):
    """Cut the window where the firmware would have cut it.

    On the wand there is no choice about which 0.9 s gets classified: the
    window is whatever had accumulated at the moment movement stopped, so the
    gesture always sits hard against the end of it. Choosing the *busiest* 0.9 s
    of a recording instead - the obvious thing, and what this used to do - puts
    the gesture somewhere near the middle, and where in the window a gesture
    sits then becomes a feature the model can learn. It is a feature that means
    nothing, because at runtime the position is fixed by the trigger rather than
    by how quickly somebody reacted to a prompt.

    So this runs the firmware's own detector over the recording and takes the
    window ending at the first movement it would have acted on. If the gesture
    started too early for a full window to precede it, the beginning is padded
    with the first sample - faithful, because the wand was still before the
    gesture and that is what the ring buffer would have held.

    Returns None if no movement crosses the threshold, which is normal for the
    calmer parts of negative.txt.
    """
    activity = runtime_activity(window)

    moving = False
    for i, value in enumerate(activity):
        if value >= ACTIVE_GATE:
            moving = True
        elif moving and value < QUIET_GATE:
            start = i + 1 - WINDOW
            if start >= 0:
                return window[start:i + 1]
            pad = np.repeat(window[:1], -start, axis=0)
            return np.concatenate([pad, window[:i + 1]])
    return None


def crop_window(recording):
    """Reduce a recording to the WINDOW the model is trained on.

    Prefers the window the firmware would have classified; falls back to the
    busiest one when nothing crossed the movement threshold, so that still and
    low-energy negative recordings are still usable.
    """
    if len(recording) <= WINDOW:
        return recording

    triggered = crop_at_runtime_trigger(recording)
    if triggered is not None:
        return triggered

    # Nothing crossed the threshold, so take the busiest window instead, scored
    # with the same measure the firmware uses. Measuring change from one sample
    # to the next rather than deviation from an average matters here: a mean
    # taken over the whole recording is dragged up by the movement itself, which
    # makes the still parts either side look active too, and the candidates then
    # come out so close together that noise decides between them.
    activity = runtime_activity(recording)

    # Prefix sums turn "total activity of every candidate window" into one
    # subtraction per position.
    cumulative = np.concatenate([[0.0], np.cumsum(activity)])
    totals = cumulative[WINDOW:] - cumulative[:-WINDOW]
    start = int(np.argmax(totals))
    return recording[start:start + WINDOW]


def count_clipped(recording):
    """Samples where any axis ran out of range, so its real value is unknown."""
    return int((np.abs(recording) >= CLIP_LEVEL).any(axis=1).sum())


def to_units(window):
    """Raw counts -> g and deg/s, so both sensors land on a similar scale."""
    scaled = window.copy()
    scaled[:, 0:3] /= ACCEL_LSB_PER_G
    scaled[:, 3:6] /= GYRO_LSB_PER_DPS
    return scaled


def load_dataset(data_dir, channels):
    """Load every <label>.txt in data_dir. File name is the class name."""
    files = sorted(pathlib.Path(data_dir).glob("*.txt"))
    if not files:
        sys.exit(f"No .txt capture files found in {data_dir}/")

    xs, ys, labels = [], [], []
    for path in files:
        windows = parse_capture_file(path)
        if not windows:
            print(f"  ! {path.name}: no complete windows, skipping", file=sys.stderr)
            continue
        label = path.stem.lower()
        labels.append(label)
        clipped = 0
        for w in windows:
            clipped += count_clipped(w)
            # Units before cropping: the crop reproduces the firmware's movement
            # detector, whose thresholds are in g and deg/s.
            xs.append(crop_window(to_units(w))[:, :channels])
            ys.append(len(labels) - 1)

        note = ""
        if clipped:
            # Occasional clipping costs little; a lot of it means the peaks of
            # every hard swing are flat-topped, and flat tops look alike.
            share = 100.0 * clipped / (len(windows) * CAPTURE)
            note = f"   ({share:.1f}% of samples out of range)"
        print(f"  {label:<10} {len(windows):>4} gestures{note}")

    x = np.stack(xs).astype(np.float32)
    y = np.array(ys, dtype=np.int32)
    return x, y, labels


def shift_window(window, n):
    """Slide a window along in time, holding the end values to fill the gap.

    Not np.roll: wrapping would bring the end of a gesture round to the start
    and invent a movement that never happened. Repeating the first or last
    sample extends the still part instead, which is what really surrounds a
    gesture.
    """
    if n == 0:
        return window
    out = np.empty_like(window)
    if n > 0:
        out[:n] = window[0]
        out[n:] = window[:-n]
    else:
        out[n:] = window[-1]
        out[:n] = window[-n:]
    return out


def augment(x, y, factor, rng):
    """Expand the set with shifted, rescaled and slightly noisy copies.

    Three things vary between recording and casting, and each gets its own
    treatment:

    - *When* the gesture falls in the window. On-device the window slides
      continuously, so the gesture can be anywhere in it, while cropping has
      put every training example neatly in the middle. Shifting by up to a
      fifth of the window covers the difference.
    - *How hard* it was swung. Scaling the magnitude keeps the model reading
      the shape of a gesture rather than its size - within limits, since a
      big enough change would turn a spell into a fidget.
    - Sensor noise, which the added jitter stands in for.
    """
    if factor <= 0:
        return x, y

    max_shift = max(1, WINDOW // 5)
    xs, ys = [x], [y]
    for _ in range(factor):
        copies = np.stack([
            shift_window(w, int(rng.integers(-max_shift, max_shift + 1))) for w in x
        ])
        copies *= rng.uniform(0.85, 1.15, (len(copies), 1, 1)).astype(np.float32)
        copies += rng.normal(0.0, 0.02, copies.shape).astype(np.float32)
        xs.append(copies.astype(np.float32))
        ys.append(y)
    return np.concatenate(xs), np.concatenate(ys)


def class_weights(y, labels, negative_scale):
    """Even out the classes, then scale `negative` deliberately.

    A rejection class has far more ground to cover than any one gesture, so
    `negative.txt` ends up holding more recordings than the spells - here twice
    as many. Left alone, that imbalance simply teaches the model that "not a
    spell" is the right answer twice as often as it really is, and genuine
    gestures get absorbed into it. Weighting each class by the inverse of how
    much data it brought removes the thumb from the scale.

    `negative_scale` then tunes what the balance cannot: how cautious to be.
    Below 1.0 the model is readier to call something a spell, which trades
    missed casts for false ones.
    """
    counts = np.bincount(y, minlength=len(labels)).astype(np.float64)
    total = counts.sum()
    weights = {}
    for i, n in enumerate(counts):
        if n == 0:
            continue
        w = total / (len(counts) * n)
        if labels[i] == "negative":
            w *= negative_scale
        weights[i] = w
    return weights


def build_model(tf, channels, n_classes, batch_size=None):
    """Small CNN using only operators MicroFlow can run.

    `batch_size` is left free for training and pinned to 1 for export - see the
    export code for why that matters.
    """
    layers = tf.keras.layers

    inp = layers.Input(shape=(WINDOW, channels, 1), batch_size=batch_size)
    # Kernel spans every channel at once, so the first layer can key on how the
    # axes move together rather than each axis in isolation.
    x = layers.Conv2D(8, (4, channels), padding="same", activation="relu")(inp)
    x = layers.AveragePooling2D((3, 1))(x)
    x = layers.Dropout(0.2)(x)
    x = layers.Conv2D(16, (4, 1), padding="same", activation="relu")(x)
    x = layers.AveragePooling2D((3, 1))(x)
    x = layers.Dropout(0.2)(x)

    # Deliberately NOT layers.Flatten(): Keras exports Flatten as a reshape whose
    # target shape is computed at runtime, which pulls in a SHAPE operator that
    # MicroFlow cannot execute. Naming the size explicitly keeps the reshape
    # static, so the exported graph holds a plain RESHAPE.
    flat = int(np.prod(x.shape[1:]))
    x = layers.Reshape((flat,))(x)

    x = layers.Dense(16, activation="relu")(x)
    x = layers.Dropout(0.2)(x)
    out = layers.Dense(n_classes, activation="softmax")(x)
    return tf.keras.Model(inp, out)


# Everything MicroFlow can execute on-device. QUANTIZE/DEQUANTIZE are tolerated
# because a fully int8 model should not contain them at all - if they show up,
# the conversion did not go fully integer.
MICROFLOW_OPS = {
    "CONV_2D",
    "DEPTHWISE_CONV_2D",
    "FULLY_CONNECTED",
    "AVERAGE_POOL_2D",
    "RESHAPE",
    "SOFTMAX",
    "RELU",
    "RELU6",
}


def check_ops(tf, tflite):
    """Fail loudly here rather than at firmware build time.

    A model that trains perfectly can still be unusable on-device if it contains
    an operator the inference engine lacks.
    """
    try:
        interp = tf.lite.Interpreter(model_content=tflite)
        interp.allocate_tensors()
        ops = sorted({d["op_name"] for d in interp._get_ops_details()})
    except Exception as exc:  # older/newer TF without the private helper
        print(f"(could not inspect operators: {exc})")
        return

    # DELEGATE is the interpreter's XNNPACK acceleration showing up in the op
    # list - a property of how we are inspecting the model, not of the model.
    ops = [o for o in ops if o != "DELEGATE"]

    print("Operators used:", ", ".join(ops))
    bad = [o for o in ops if o not in MICROFLOW_OPS]
    if bad:
        print(
            f"\n!! {', '.join(bad)} is not supported by MicroFlow - the firmware "
            f"will refuse to build.\n"
            f"   Supported: {', '.join(sorted(MICROFLOW_OPS))}",
            file=sys.stderr,
        )
    else:
        print("All operators are supported by MicroFlow.")

    # MicroFlow's fully_connected takes exactly one quantisation scale per
    # tensor, so a per-channel tensor stops the firmware compiling.
    multi = [
        t["name"]
        for t in interp.get_tensor_details()
        if len(t.get("quantization_parameters", {}).get("scales", [])) > 1
    ]
    if multi:
        print(
            f"\n!! {len(multi)} tensor(s) are quantised per-channel, e.g. "
            f"{multi[0][:60]}\n"
            f"   MicroFlow needs per-tensor quantisation - the firmware will "
            f"refuse to build.",
            file=sys.stderr,
        )
    else:
        print("Quantisation is per-tensor, as MicroFlow requires.")


def confusion(y_true, y_pred, n):
    m = np.zeros((n, n), dtype=int)
    for t, p in zip(y_true, y_pred):
        m[t, p] += 1
    return m


def print_confusion(matrix, labels):
    width = max(len(l) for l in labels) + 1
    print("\nConfusion matrix (rows = actual, cols = predicted):")
    print(" " * width + "".join(f"{l[:6]:>7}" for l in labels))
    for label, row in zip(labels, matrix):
        print(f"{label:<{width}}" + "".join(f"{v:>7}" for v in row))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data-dir", default="data")
    ap.add_argument("--out", default="model")
    ap.add_argument("--channels", type=int, default=6, choices=(3, 6),
                    help="3 = accel only (like MagicWand), 6 = accel + gyro")
    ap.add_argument("--epochs", type=int, default=60)
    ap.add_argument("--negative-weight", type=float, default=1.0,
                    help="how much the negative class counts once the classes "
                         "have been balanced. Below 1.0 makes the model readier "
                         "to call a movement a spell, at the cost of more false "
                         "casts; try 0.7 if real gestures are being swallowed.")
    ap.add_argument("--augment", type=int, default=3,
                    help="extra augmented copies of the training set (0 = off)")
    ap.add_argument("--seed", type=int, default=1)
    args = ap.parse_args()

    import tensorflow as tf

    rng = np.random.default_rng(args.seed)
    tf.keras.utils.set_random_seed(args.seed)

    print("Loading captures:")
    x, y, labels = load_dataset(args.data_dir, args.channels)
    n_classes = len(labels)
    print(f"\n{len(x)} gestures, {n_classes} classes, {args.channels} channels")

    if len(x) < n_classes * 5:
        print("! Very little data - expect poor accuracy. Record more.", file=sys.stderr)

    # Stratified split so every class appears in train/val/test.
    train_i, val_i, test_i = [], [], []
    for c in range(n_classes):
        idx = np.where(y == c)[0]
        rng.shuffle(idx)
        n_test = max(1, int(0.15 * len(idx)))
        n_val = max(1, int(0.15 * len(idx)))
        test_i += list(idx[:n_test])
        val_i += list(idx[n_test:n_test + n_val])
        train_i += list(idx[n_test + n_val:])

    x_train, y_train = x[train_i], y[train_i]
    x_val, y_val = x[val_i], y[val_i]
    x_test, y_test = x[test_i], y[test_i]

    x_train, y_train = augment(x_train, y_train, args.augment, rng)
    print(f"train {len(x_train)}  val {len(x_val)}  test {len(x_test)}")

    # Add the trailing 1 channel dimension the Conv2D layers expect.
    x_train = x_train[..., None]
    x_val = x_val[..., None]
    x_test = x_test[..., None]

    model = build_model(tf, args.channels, n_classes)
    model.compile(optimizer="adam",
                  loss="sparse_categorical_crossentropy",
                  metrics=["accuracy"])
    model.summary()

    weights = class_weights(y_train, labels, args.negative_weight)
    print("\nclass weights:")
    for i, label in enumerate(labels):
        print(f"  {label:<10}{weights.get(i, 0.0):.3f}")

    model.fit(
        x_train, y_train,
        validation_data=(x_val, y_val),
        class_weight=weights,
        epochs=args.epochs,
        batch_size=32,
        verbose=2,
        callbacks=[
            tf.keras.callbacks.EarlyStopping(
                monitor="val_accuracy", patience=15, restore_best_weights=True
            )
        ],
    )

    loss, acc = model.evaluate(x_test, y_test, verbose=0)
    print(f"\nTest accuracy: {acc:.1%}")
    pred = model.predict(x_test, verbose=0).argmax(axis=1)
    print_confusion(confusion(y_test, pred, n_classes), labels)

    # --- Export: full int8 quantisation, which is what MicroFlow runs ---
    def representative():
        for sample in x_train[:200]:
            yield [sample[None].astype(np.float32)]

    # Export from a copy whose batch dimension is pinned to 1. With a dynamic
    # batch, Keras builds even a fixed-size reshape out of
    # tf.shape -> strided_slice -> pack, dragging in operators MicroFlow cannot
    # execute; a concrete batch lets the converter fold that into a constant
    # reshape. The device only ever classifies one window at a time anyway.
    #
    # Done by copying weights into a second model rather than converting a
    # concrete function, because that route leaves the weights as resource
    # variables and quantisation calibration then fails on READ_VARIABLE.
    export_model = build_model(tf, args.channels, n_classes, batch_size=1)
    export_model.set_weights(model.get_weights())

    converter = tf.lite.TFLiteConverter.from_keras_model(export_model)
    converter.optimizations = [tf.lite.Optimize.DEFAULT]

    # Force per-tensor (one scale per tensor) instead of TFLite's default
    # per-channel weight quantisation. MicroFlow's fully_connected only accepts a
    # single quantisation scale, so a per-channel Dense layer fails to compile.
    # Its conv_2d does handle per-channel, but there is no converter switch for
    # "convolutions only", and the accuracy difference on a model this small is
    # negligible.
    converter._experimental_disable_per_channel = True
    if hasattr(converter, "_experimental_disable_per_channel_quantization_for_dense_layers"):
        converter._experimental_disable_per_channel_quantization_for_dense_layers = True
    converter.representative_dataset = representative
    converter.target_spec.supported_ops = [tf.lite.OpsSet.TFLITE_BUILTINS_INT8]
    converter.inference_input_type = tf.int8
    converter.inference_output_type = tf.int8
    tflite = converter.convert()

    out_dir = pathlib.Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)
    model_path = out_dir / "spells.tflite"
    model_path.write_bytes(tflite)
    (out_dir / "labels.txt").write_text("\n".join(labels) + "\n")

    print(f"\nWrote {model_path} ({len(tflite)} bytes)")
    print(f"Wrote {out_dir / 'labels.txt'}: {', '.join(labels)}")

    check_ops(tf, tflite)

    # Sanity-check the quantised model, since quantisation can cost accuracy.
    interp = tf.lite.Interpreter(model_content=tflite)
    interp.allocate_tensors()
    inp, out = interp.get_input_details()[0], interp.get_output_details()[0]
    scale, zero = inp["quantization"]
    correct = 0
    for sample, truth in zip(x_test, y_test):
        q = np.clip(np.round(sample / scale + zero), -128, 127).astype(np.int8)
        interp.set_tensor(inp["index"], q[None])
        interp.invoke()
        if interp.get_tensor(out["index"])[0].argmax() == truth:
            correct += 1
    print(f"Quantised model test accuracy: {correct / len(x_test):.1%}")


if __name__ == "__main__":
    main()
