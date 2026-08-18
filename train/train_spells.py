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

# Samples per gesture window - must match WINDOW in src/bin/capture.rs.
WINDOW = 128
# Raw counts -> physical units, at the MPU6050's default ranges.
ACCEL_LSB_PER_G = 16384.0
GYRO_LSB_PER_DPS = 131.0


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
    """Split one capture file into a list of (WINDOW, 6) float arrays.

    The firmware prints a separator line before each gesture and comment lines
    starting with '#', so windows are whatever sits between separators.
    """
    windows, current = [], []

    def flush():
        if not current:
            return
        if len(current) == WINDOW:
            windows.append(np.array(current, dtype=np.float32))
        else:
            print(
                f"  ! {path.name}: dropped a window with {len(current)} rows "
                f"(expected {WINDOW}) - recording probably interrupted",
                file=sys.stderr,
            )

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
    return windows


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
        for w in windows:
            xs.append(to_units(w)[:, :channels])
            ys.append(len(labels) - 1)
        print(f"  {label:<10} {len(windows):>4} gestures")

    x = np.stack(xs).astype(np.float32)
    y = np.array(ys, dtype=np.int32)
    return x, y, labels


def augment(x, y, factor, rng):
    """Expand the set with time-shifted, slightly noisy copies.

    Hand-recorded gestures are few and never start at exactly the same instant,
    so shifting teaches the model not to depend on precise alignment.
    """
    if factor <= 0:
        return x, y
    xs, ys = [x], [y]
    for _ in range(factor):
        shifted = np.stack([np.roll(w, rng.integers(-12, 13), axis=0) for w in x])
        shifted += rng.normal(0.0, 0.02, shifted.shape).astype(np.float32)
        xs.append(shifted.astype(np.float32))
        ys.append(y)
    return np.concatenate(xs), np.concatenate(ys)


def build_model(tf, channels, n_classes):
    """Small CNN using only operators MicroFlow can run."""
    layers = tf.keras.layers
    return tf.keras.Sequential(
        [
            layers.Input(shape=(WINDOW, channels, 1)),
            # Kernel spans every channel at once, so the first layer can key on
            # how the axes move together rather than each axis in isolation.
            layers.Conv2D(8, (4, channels), padding="same", activation="relu"),
            layers.AveragePooling2D((3, 1)),
            layers.Dropout(0.2),
            layers.Conv2D(16, (4, 1), padding="same", activation="relu"),
            layers.AveragePooling2D((3, 1)),
            layers.Dropout(0.2),
            layers.Flatten(),  # exported as Reshape
            layers.Dense(16, activation="relu"),
            layers.Dropout(0.2),
            layers.Dense(n_classes, activation="softmax"),
        ]
    )


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

    model.fit(
        x_train, y_train,
        validation_data=(x_val, y_val),
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

    converter = tf.lite.TFLiteConverter.from_keras_model(model)
    converter.optimizations = [tf.lite.Optimize.DEFAULT]
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
