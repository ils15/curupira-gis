#!/usr/bin/env python3
"""
gen_bench_fixtures.py — dataset de decisão da Fase 3 (DISCOVERY.md»Bench
Dataset Spec). Clean-room: NADA deriva do bundle legado — dados 100%
sintéticos, determinísticos (LCG, seed 42).

Saída (default: testdata/bench/ — GERADO, não commitado; o script e o
manifest.json são versionados):

  bench_polys.shp/.shx/.dbf/.prj   10.000 polígonos irregulares, EPSG:4674,
                                   bbox lon [-73,-50] / lat [-10,2],
                                   attrs: id, nome, area_ha, classe
  bench_points.csv                 50.000 linhas lon,lat (EPSG:4674) + 5 attrs
  bench_raster.tif                 GeoTIFF 1024x1024, 4 bandas UInt16,
                                   tiled 256x256, LZW, nodata=0, EPSG:4674
  bench_raster_stress.tif          (--stress) 8192x8192, 1 banda UInt16
  manifest.json                    sha256 + bytes + rows/dims + params

Requisitos: python3 com osgeo (system python3-gdal 3.8+). Sem deps extras.

Uso:
    python3 scripts/gen_bench_fixtures.py [--outdir testdata/bench] \
        [--seed 42] [--stress]
"""

import argparse
import csv
import hashlib
import json
import math
import sys
import time
from pathlib import Path

try:
    from osgeo import gdal, ogr, osr
except ImportError:  # pragma: no cover
    sys.exit("osgeo (python3-gdal) não encontrado: instale python3-gdal / libgdal")

SRID = 4674
LON_MIN, LON_MAX = -73.0, -50.0
LAT_MIN, LAT_MAX = -10.0, 2.0
N_POLYS = 10_000
N_POINTS = 50_000
CLASSES = ["floresta", "pastagem", "agricultura", "agua", "desmatamento"]

gdal.UseExceptions()
ogr.UseExceptions()


class Lcg:
    """LCG determinístico (mesmo gerador do testdata/gen_fixtures.py)."""

    def __init__(self, seed: int):
        self.state = seed & 0x7FFFFFFF

    def next(self) -> int:
        self.state = (self.state * 1103515245 + 12345) & 0x7FFFFFFF
        return self.state

    def unit(self) -> float:
        return self.next() / 0x7FFFFFFF

    def between(self, a: float, b: float) -> float:
        return a + (b - a) * self.unit()

    def int(self, lo: int, hi: int) -> int:
        return lo + self.next() % (hi - lo + 1)


def srs_4674():
    srs = osr.SpatialReference()
    srs.ImportFromEPSG(SRID)
    return srs


def irregular_polygon(rng: Lcg, cx: float, cy: float, max_r: float):
    """Estrela com raios variáveis → polígono simples (sem auto-interseção)."""
    n = rng.int(8, 20)
    ring = ogr.Geometry(ogr.wkbLinearRing)
    step = 2 * math.pi / n
    base_r = max_r * rng.between(0.45, 0.7)
    for i in range(n):
        angle = i * step
        r = min(max_r, base_r * rng.between(0.55, 1.45))
        ring.AddPoint_2D(cx + r * math.cos(angle), cy + r * math.sin(angle))
    ring.AddPoint_2D(ring.GetX(0), ring.GetY(0))
    poly = ogr.Geometry(ogr.wkbPolygon)
    poly.AddGeometry(ring)
    return poly


def shoelace_ha(ring) -> float:
    pts = [ring.GetPoint_2D(i) for i in range(ring.GetPointCount() - 1)]
    s = 0.0
    n = len(pts)
    for i in range(n):
        x1, y1 = pts[i]
        x2, y2 = pts[(i + 1) % n]
        s += x1 * y2 - x2 * y1
    return abs(s) / 2.0 * 100.0  # graus² ~ aproximado; fator p/ hectare de demo


def gen_polygons(outdir: Path, seed: int) -> Path:
    t0 = time.time()
    rng = Lcg(seed)
    driver = ogr.GetDriverByName("ESRI Shapefile")
    shp = outdir / "bench_polys.shp"
    ds = driver.CreateDataSource(str(shp))
    layer = ds.CreateLayer("bench_polys", srs_4674(), ogr.wkbPolygon)
    for name, typ, width in [
        ("id", ogr.OFTInteger, 0),
        ("nome", ogr.OFTString, 24),
        ("area_ha", ogr.OFTReal, 0),
        ("classe", ogr.OFTString, 12),
    ]:
        fd = ogr.FieldDefn(name, typ)
        if width:
            fd.SetWidth(width)
        layer.CreateField(fd)

    # Grades com jitter: cobre o bbox inteiro sem sobrepor demais.
    cols = 100
    rows = N_POLYS // cols
    dx = (LON_MAX - LON_MIN) / cols
    dy = (LAT_MAX - LAT_MIN) / rows
    feat = ogr.Feature(layer.GetLayerDefn())
    n = 0
    for r in range(rows):
        for c in range(cols):
            cx = LON_MIN + (c + 0.5) * dx + rng.between(-0.3, 0.3) * dx
            cy = LAT_MIN + (r + 0.5) * dy + rng.between(-0.3, 0.3) * dy
            poly = irregular_polygon(rng, cx, cy, min(dx, dy) * 0.55)
            if poly is None:
                continue
            f = feat.Clone()
            f.SetGeometry(poly)
            f.SetField("id", n)
            f.SetField("nome", f"poligonal_{n:05d}")
            f.SetField("area_ha", round(shoelace_ha(poly.GetGeometryRef(0)), 3))
            f.SetField("classe", CLASSES[n % len(CLASSES)])
            layer.CreateFeature(f)
            n += 1
    ds = None
    print(f"  bench_polys.shp: {n} polígonos em {time.time()-t0:.1f}s")
    return shp


def gen_points(out: Path, seed: int) -> Path:
    t0 = time.time()
    rng = Lcg(seed + 1)
    with out.open("w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh)
        w.writerow(["lon", "lat", "id", "nome", "classe", "valor", "flag"])
        for i in range(N_POINTS):
            lon = rng.between(LON_MIN, LON_MAX)
            lat = rng.between(LAT_MIN, LAT_MAX)
            w.writerow([
                f"{lon:.6f}",
                f"{lat:.6f}",
                i,
                f"ponto_{i:06d}",
                CLASSES[(i // 7) % len(CLASSES)],
                round(rng.between(0.0, 1000.0), 3),
                i % 2,
            ])
    print(f"  {out.name}: {N_POINTS} linhas em {time.time()-t0:.1f}s")
    return out


def gen_raster(out: Path, seed: int, width: int, height: int, bands: int, tile: int):
    t0 = time.time()
    # numpy via python3-gdal (presente no host; golden dos testes usa o mesmo
    # caminho). Sem numpy, o loop puro abaixo continuaria correto — só lento.
    import numpy as np  # noqa: PLC0415 (import local: só o gerador usa)

    rng = Lcg(seed + 2)
    driver = gdal.GetDriverByName("GTiff")
    ds = driver.Create(
        str(out), width, height, bands, gdal.GDT_UInt16,
        options=["TILED=YES", f"BLOCKXSIZE={tile}", f"BLOCKYSIZE={tile}", "COMPRESS=LZW"],
    )
    ds.SetGeoTransform([LON_MIN, (LON_MAX - LON_MIN) / width, 0.0,
                        LAT_MAX, 0.0, -(LAT_MAX - LAT_MIN) / height])
    ds.SetProjection(srs_4674().ExportToWkt())
    xs = np.arange(width, dtype=np.uint16)
    ys = np.arange(height, dtype=np.uint16).reshape(-1, 1)
    for b in range(1, bands + 1):
        band = ds.GetRasterBand(b)
        band.SetNoDataValue(0)
        base = (32768 * xs / width + 16384 * ys / height + b * 4096).astype(np.uint32)
        # Ruído seeded por blocos de linha (LCG determinístico).
        noise = np.fromiter(
            (rng.next() % 4096 for _ in range(width * height)), dtype=np.uint32,
            count=width * height,
        ).reshape(height, width)
        band.WriteArray(((base + noise) % 65536).astype(np.uint16))
    ds.FlushCache()
    ds = None
    print(f"  {out.name}: {width}x{height}x{bands}b em {time.time()-t0:.1f}s")
    return out


def sha256_of(p: Path) -> str:
    h = hashlib.sha256()
    with p.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--outdir", type=Path, default=Path(__file__).resolve().parent.parent / "testdata" / "bench")
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--stress", action="store_true", help="gera também o raster 8192²")
    args = ap.parse_args()
    args.outdir.mkdir(parents=True, exist_ok=True)

    print(f"Gerando dataset de bench (seed {args.seed}) em {args.outdir} …")
    shp = gen_polygons(args.outdir, args.seed)
    points = gen_points(args.outdir / "bench_points.csv", args.seed)
    raster = gen_raster(args.outdir / "bench_raster.tif", args.seed, 1024, 1024, 4, 256)
    manifest = {
        "params": {"seed": args.seed, "n_polys": N_POLYS, "n_points": N_POINTS},
        "files": [],
    }
    for p, rows_dims in [
        (shp, {"rows": N_POLYS}),
        (points, {"rows": N_POINTS}),
        (raster, {"dims": [1024, 1024, 4]}),
    ]:
        manifest["files"].append({
            "arquivo": p.name,
            "sha256": sha256_of(p),
            "bytes": p.stat().st_size,
            **rows_dims,
        })
    if args.stress:
        stress = gen_raster(args.outdir / "bench_raster_stress.tif", args.seed, 8192, 8192, 1, 256)
        manifest["files"].append({
            "arquivo": stress.name,
            "sha256": sha256_of(stress),
            "bytes": stress.stat().st_size,
            "dims": [8192, 8192, 1],
        })
    mpath = args.outdir / "manifest.json"
    mpath.write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"  manifest.json com {len(manifest['files'])} entrada(s).")


if __name__ == "__main__":
    main()
