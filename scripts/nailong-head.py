#!/usr/bin/env python3
"""重训奶龙拦截的分类头（src/plugins/nailong/head.bin）。做法与数据来源见 docs/nailong.md。

    python3 scripts/nailong-head.py <vision_model.onnx> <数据目录> [--eval]

数据目录下按类别放图（子目录随意，jpg/png/webp/gif；动图取第一帧与中间一帧）：

    nailong/…   奶龙
    naiwa/…     奶蛙
    other/…     其他：通用表情包、黄色易混图
    test/{nailong,naiwa,other}/…   可选：留出集，带 --eval 时只用来打分、不参与训练

向量缓存在 <数据目录>/.vectors.npz，图没变就不重算。需要 onnxruntime、numpy、pillow。
预处理要与 src/plugins/nailong/model.rs 一致：透明垫白、等比缩进 256 正方形居中、/255。
"""
import glob, hashlib, os, struct, sys

import numpy as np
import onnxruntime as ort
from PIL import Image

CLASSES = ["other", "nailong", "naiwa"]
HIDDEN, EPOCHS, LR, DECAY, BATCH, DROPOUT, SEED = 256, 60, 1e-3, 1e-4, 128, 0.2, 10
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "../src/plugins/nailong/head.bin")


def frames(path):
    image = Image.open(path)
    count = getattr(image, "n_frames", 1)
    for index in sorted({0, count // 2}):
        image.seek(index)
        rgba = image.convert("RGBA")
        canvas = Image.new("RGBA", rgba.size, (255, 255, 255, 255))
        canvas.alpha_composite(rgba)
        rgb = canvas.convert("RGB")
        scale = 256 / max(rgb.size)
        rgb = rgb.resize((max(1, round(rgb.width * scale)), max(1, round(rgb.height * scale))), Image.BICUBIC)
        square = Image.new("RGB", (256, 256), (255, 255, 255))
        square.paste(rgb, ((256 - rgb.width) // 2, (256 - rgb.height) // 2))
        yield (np.asarray(square, dtype=np.float32) / 255).transpose(2, 0, 1)[None]


def embed(model, root):
    files = sorted(f for f in glob.glob(os.path.join(root, "**", "*"), recursive=True)
                   if f.rsplit(".", 1)[-1].lower() in ("jpg", "jpeg", "png", "webp", "gif", "bmp"))
    cache_path = os.path.join(root, ".vectors.npz")
    cache = dict(np.load(cache_path, allow_pickle=True)) if os.path.exists(cache_path) else {}
    session = ort.InferenceSession(model)
    vectors, labels, held_out = [], [], []
    fresh = {}
    for index, path in enumerate(files):
        relative = os.path.relpath(path, root)
        parts = relative.split(os.sep)
        test = parts[0] == "test"
        label = parts[1] if test else parts[0]
        if label not in CLASSES:
            continue
        key = hashlib.md5(open(path, "rb").read()).hexdigest()
        if key in cache:
            found = cache[key]
        else:
            try:
                found = np.array([session.run(None, {"pixel_values": x})[0][0] for x in frames(path)])
            except Exception as error:
                print(f"跳过 {relative}：{error}", file=sys.stderr)
                continue
            found /= np.linalg.norm(found, axis=1, keepdims=True)
        fresh[key] = found
        for vector in found:
            vectors.append(vector)
            labels.append(CLASSES.index(label))
            held_out.append(test)
        if index % 500 == 0:
            print(f"向量 {index}/{len(files)}", file=sys.stderr)
    np.savez(cache_path, **fresh)
    return np.array(vectors, np.float32), np.array(labels), np.array(held_out)


def softmax(z):
    z = z - z.max(1, keepdims=True)
    e = np.exp(z)
    return e / e.sum(1, keepdims=True)


def train(x, y):
    rng = np.random.default_rng(SEED)
    counts = np.bincount(y, minlength=3)
    weight = np.sqrt(counts.sum() / (3 * counts))[y]
    weight /= weight.mean()
    target = np.eye(3)[y]
    p = {"W1": rng.normal(0, np.sqrt(2 / x.shape[1]), (x.shape[1], HIDDEN)).astype(np.float32),
         "b1": np.zeros(HIDDEN, np.float32),
         "W2": rng.normal(0, np.sqrt(1 / HIDDEN), (HIDDEN, 3)).astype(np.float32),
         "b2": np.zeros(3, np.float32)}
    m = {k: np.zeros_like(v) for k, v in p.items()}
    v = {k: np.zeros_like(v) for k, v in p.items()}
    step = 0
    for _ in range(EPOCHS):
        order = rng.permutation(len(y))
        for start in range(0, len(y), BATCH):
            i = order[start:start + BATCH]
            pre = x[i] @ p["W1"] + p["b1"]
            mask = (rng.random(pre.shape) > DROPOUT) / (1 - DROPOUT)
            hidden = np.maximum(pre, 0) * mask
            g = (softmax(hidden @ p["W2"] + p["b2"]) - target[i]) * weight[i][:, None] / len(i)
            grads = {"W2": hidden.T @ g, "b2": g.sum(0)}
            back = g @ p["W2"].T * mask * (pre > 0)
            grads["W1"], grads["b1"] = x[i].T @ back, back.sum(0)
            step += 1
            for k in p:
                grad = grads[k] + (DECAY * p[k] if k.startswith("W") else 0)
                m[k] = 0.9 * m[k] + 0.1 * grad
                v[k] = 0.999 * v[k] + 0.001 * grad * grad
                p[k] -= LR * (m[k] / (1 - 0.9 ** step)) / (np.sqrt(v[k] / (1 - 0.999 ** step)) + 1e-8)
    return p


def predict(p, x):
    return softmax(np.maximum(x @ p["W1"] + p["b1"], 0) @ p["W2"] + p["b2"])


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    x, y, held_out = embed(sys.argv[1], sys.argv[2])
    if "--eval" in sys.argv and held_out.any():
        p = train(x[~held_out], y[~held_out])
        hit = predict(p, x[held_out])[:, 1:].max(1)
        for threshold in (0.6, 0.7, 0.8, 0.9):
            line = [f"阈值 {threshold}"]
            for c, name in enumerate(CLASSES):
                chosen = y[held_out] == c
                rate = (hit[chosen] >= threshold).mean() if chosen.any() else float("nan")
                line.append(f"{name} {'误判' if c == 0 else '认出'} {rate:.1%}")
            print("  ".join(line))
    p = train(x, y)
    blob = b"NLH1" + struct.pack("<III", x.shape[1], HIDDEN, 3)
    blob += p["W1"].T.astype("<f4").tobytes() + p["b1"].astype("<f4").tobytes()
    blob += p["W2"].T.astype("<f4").tobytes() + p["b2"].astype("<f4").tobytes()
    open(OUT, "wb").write(blob)
    print(f"写入 {os.path.relpath(OUT)}（{len(blob)} 字节，{len(y)} 个向量）")


if __name__ == "__main__":
    main()
