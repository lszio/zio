"""Reader for the W00 acceptance containers (GVD1).

Shared by the trainer and the teacher service so both agree on exactly
what a sample is — in particular, on the rule that a missing modality is
a declared absence, never a zero-filled stand-in.
"""

from __future__ import annotations

import json
import struct
from dataclasses import dataclass
from pathlib import Path

SAMPLE = struct.Struct("<IBBBBff")
PIXELS = 256
LABEL_NONE = 0xFF


@dataclass
class Observation:
    scene_id: int
    image_present: bool
    numeric_present: bool
    label: int
    flags: int
    readings: list[float]
    pixels: bytes

    @property
    def decidable(self) -> bool:
        return self.image_present and self.numeric_present

    @staticmethod
    def from_parts(parts: list[dict]) -> "Observation":
        """Rebuild an observation from a teacher request's content parts."""
        pixels = bytes(PIXELS)
        readings = [0.0, 0.0]
        image_present = numeric_present = False
        for part in parts:
            kind = part.get("kind")
            if kind == "reference" and part.get("media_type", "").startswith("image/"):
                image_present = True
                raw = part.get("pixels", [])
                pixels = bytes(int(v) for v in raw[:PIXELS])
            elif kind == "numbers":
                numeric_present = True
                values = part.get("values", [])
                for i, v in enumerate(values[:2]):
                    readings[i] = float(v)
        return Observation(
            scene_id=0,
            image_present=image_present,
            numeric_present=numeric_present,
            label=LABEL_NONE,
            flags=0,
            readings=readings,
            pixels=pixels,
        )


def load_split(path: Path) -> list[Observation]:
    blob = path.read_bytes()
    magic, hlen = struct.unpack_from("<4sI", blob, 0)
    assert magic == b"GVD1", f"{path}: bad magic"
    head = 8 + hlen
    header = json.loads(blob[8:head].decode("utf-8"))
    stride = header["record_bytes"]
    rows: list[Observation] = []
    pos = head
    while pos < len(blob):
        scene_id, image_present, numeric_present, label, flags, r1, r2 = SAMPLE.unpack_from(blob, pos)
        pixels = blob[pos + SAMPLE.size: pos + SAMPLE.size + PIXELS]
        rows.append(Observation(
            scene_id=scene_id,
            image_present=bool(image_present),
            numeric_present=bool(numeric_present),
            label=label,
            flags=flags,
            readings=[r1, r2],
            pixels=pixels,
        ))
        pos += stride
    return rows


def load_labels(path: Path) -> dict[int, int | None]:
    """Holdout oracle: index → answer, or None for an undecidable row."""
    blob = json.loads(path.read_text(encoding="utf-8"))
    return {entry["index"]: entry["answer"] for entry in blob["expected"]}
