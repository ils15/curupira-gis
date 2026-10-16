#!/usr/bin/env python3
"""
gen_fixtures.py — fixtures sintéticos versionados do terraamazon-cleanroom-rust.

Clean-room: NADA aqui deriva do bundle legado — todos os dados são gerados
deterministicamente (seed 42) por este script. Saída (default: testdata/):

  fixtures_vector.shp/.shx/.dbf/.prj  200 polígonos irregulares, EPSG:4674
  fixtures_vector.geojson             mesmo conteúdo, GeoJSON
  fixtures_vector.gpkg                mesmo conteúdo, GeoPackage
  fixtures_points.csv                 500 pontos lon/lat (CSV-XY)
  fixtures_raster.tif                 256×256, 4 bandas UInt16, tiled 64
  manifest.json                       sha256 + bytes + params

Requisitos: python3 com `osgeo` (system python3-gdal 3.8+ — presente no host;
em CI: apt install python3-gdal). Sem dependências além da stdlib.

Uso:
    python3 testdata/gen_fixtures.py [--outdir testdata] [--seed 42]
"""

import argparse
import csv
import hashlib
import json
import math
import random  # noqa: F401 (usado só se alguém ajustar o script p/ aleatório não-LCG)
import struct
import sys
from pathlib import Path

try:
    from osgeo import gdal, ogr, osr
except ImportError:  # pragma: no cover
    sys.exit("osgeo (python3-gdal) não encontrado: instale python3-gdal / libgdal")

SRID = 4674  # SIRGAS 2000 (geográfico, lon/lat)
LON_MIN, LON_MAX = -73.0, -50.0
LAT_MIN, LAT_MAX = -10.0, 2.0
N_POLYS = 200
N_POINTS = 500
CLASSES = ["floresta", "pastagem", "agricultura", "agua", "desmatamento"]

gdal.UseExceptions()
ogr.UseExceptions()


class Lcg:
    """LCG determinístico (evita divergência entre versões de random())."""

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


def irregular_polygon(rng: Lcg, cx: float, cy: float, max_r: float):
    """Estrela com raios positivos variáveis → polígono simples garantido."""
    n = rng.int(8, 20)
    ring = ogr.Geometry(ogr.wkbLinearRing)
    step = 2 * math.pi / n
    base_r = max_r * rng.between(0.45, 0.7)
    for i in range(n):
        angle = i * step
        r = base_r * rng.between(0.55, 1.45)
        r = min(r, max_r)
        x = cx + r * math.cos(angle)
        y = cy + r * math.sin(angle)
        ring.AddPoint_2D(x, y)
    ring.AddPoint_2D(ring.GetX(0), ring.GetY(0))
    poly = ogr.Geometry(ogr.wkbPolygon)
    poly.AddGeometry(ring)
    return poly


def make_memory_layer(srs):
    ds = ogr.GetDriverByName("Memory").CreateDataSource("mem")
    layer = ds.CreateLayer("fixtures_vector", srs, ogr.wkbPolygon)
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
    return ds, layer


def shoelace_ha(ring) -> float:
    """Área aproximada em ha — atributo sintético (1° ≈ 111 km no equador)."""
    pts = [(ring.GetX(i), ring.GetY(i)) for i in range(ring.GetPointCount())]
    s = 0.0
    for (x0, y0), (x1, y1) in zip(pts, pts[1:] + pts[:1]):
        s += x0 * y1 - x1 * y0
    deg2 = abs(s) / 2.0
    return deg2 * (111.0**2) * 100.0  # graus² → ha (aprox. demo sintética)


def build_features(seed: int):
    """(mem_ds, mem_layer) com os 200 polígonos determinísticos."""
    rng = Lcg(seed)
    srs = osr.SpatialReference()
    srs.ImportFromEPSG(SRID)
    mem_ds, layer = make_memory_layer(srs)
    for i in range(N_POLYS):
        cx = rng.between(LON_MIN + 0.6, LON_MAX - 0.6)
        cy = rng.between(LAT_MIN + 0.6, LAT_MAX - 0.6)
        poly = irregular_polygon(rng, cx, cy, 0.5)
        feat = ogr.Feature(layer.GetLayerDefn())
        feat.SetField("id", i)
        feat.SetField("nome", f"poligonal_{i:05d}"[:24])
        feat.SetField("area_ha", round(shoelace_ha(poly.GetGeometryRef(0)), 3))
        feat.SetField("classe", CLASSES[i % len(CLASSES)])
        feat.SetGeometry(poly)
        layer.CreateFeature(feat)
    return mem_ds, layer


def write_shp(mem_ds, out: Path):
    drv = ogr.GetDriverByName("ESRI Shapefile")
    drv.CopyDataSource(mem_ds, str(out))


def write_geojson(mem_ds, out: Path):
    drv = ogr.GetDriverByName("GeoJSON")
    drv.CopyDataSource(mem_ds, str(out))


def write_gpkg(mem_ds, out: Path):
    if out.exists():
        out.unlink()
    drv = ogr.GetDriverByName("GPKG")
    drv.CopyDataSource(mem_ds, str(out))


def write_points_csv(out: Path, seed: int):
    rng = Lcg(seed ^ 0x5EED)
    with out.open("w", newline="", encoding="utf-8") as f:
        w = csv.writer(f, delimiter=";")
        w.writerow(["id", "lon", "lat", "nome", "valor", "classe"])
        for i in range(N_POINTS):
            w.writerow([
                i,
                f"{rng.between(LON_MIN + 0.1, LON_MAX - 0.1):.6f}",
                f"{rng.between(LAT_MIN + 0.1, LAT_MAX - 0.1):.6f}",
                f"ponto_{i:05d}",
                rng.int(1, 9999),
                CLASSES[i % len(CLASSES)],
            ])


def write_raster(out: Path, seed: int):
    drv = gdal.GetDriverByName("GTiff")
    W = H = 256
    BLOCK = 64
    ds = drv.Create(str(out), W, H, 4, gdal.GDT_UInt16, options=["TILED=YES", f"BLOCKXSIZE={BLOCK}", f"BLOCKYSIZE={BLOCK}"])
    gt = [LON_MIN, (LON_MAX - LON_MIN) / W, 0.0, LAT_MAX, 0.0, -(LAT_MAX - LAT_MIN) / H]
    ds.SetGeoTransform(gt)
    srs = osr.SpatialReference()
    srs.ImportFromEPSG(SRID)
    ds.SetProjection(srs.ExportToWkt())
    rng = Lcg(seed ^ 0xCAFE)  # reservado p/ variações futuras
    # padrão determinístico: ruído LCG + viés por linha/banda; valores ≥ 1 (0 = nodata)
    state = seed
    for band_idx in range(1, 5):
        band = ds.GetRasterBand(band_idx)
        rows = []
        for y in range(H):
            row = []
            for _x in range(W):
                state = (state * 1103515245 + 12345) & 0x7FFFFFFF
                v = 1 + (state >> 8) % 65000
                v = (v + 7 * y + 13 * band_idx) % 65536
                row.append(v if v != 0 else 1)
            rows.append(row)
        packed = struct.pack(f"<{W*H}H", *[v for row in rows for v in row])
        band.WriteRaster(0, 0, W, H, packed, buf_type=gdal.GDT_UInt16)
        band.SetNoDataValue(0)
        band.FlushCache()
    ds.FlushCache()
    ds = None


def sha256_of(p: Path) -> str:
    h = hashlib.sha256()
    h.update(p.read_bytes())
    return h.hexdigest()


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--outdir", type=Path, default=Path(__file__).resolve().parent)
    ap.add_argument("--seed", type=int, default=42)
    args = ap.parse_args()
    out = args.outdir
    out.mkdir(parents=True, exist_ok=True)

    print(f"[fixtures] seed={args.seed} → {out}")
    mem_ds, mem_layer = build_features(args.seed)
    assert mem_layer.GetFeatureCount() == N_POLYS

    shp = out / "fixtures_vector.shp"
    geojson = out / "fixtures_vector.geojson"
    gpkg = out / "fixtures_vector.gpkg"
    csvf = out / "fixtures_points.csv"
    tif = out / "fixtures_raster.tif"

    write_shp(mem_ds, shp)
    write_geojson(mem_ds, geojson)
    write_gpkg(mem_ds, gpkg)
    write_points_csv(csvf, args.seed)
    write_raster(tif, args.seed)

    # sanity: 3 formatos concordam no N e no CRS
    for p in (shp, geojson, gpkg):
        ds = ogr.Open(str(p))
        lyr = ds.GetLayer(0)
        assert lyr.GetFeatureCount() == N_POLYS, f"{p}: count divergiu"
        assert lyr.GetSpatialRef().GetAuthorityCode(None) == str(SRID), f"{p}: CRS divergiu"
        ds = None

    manifest = {
        "seed": args.seed,
        "generator": "testdata/gen_fixtures.py",
        "clean_room": "dados sintéticos determinísticos — nada derivado do bundle legado",
        "files": [],
    }
    for p, rows in [
        (shp, N_POLYS),
        (geojson, N_POLYS),
        (gpkg, N_POLYS),
        (csvf, N_POINTS),
        (tif, "256x256x4 UInt16"),
    ]:
        manifest["files"].append({
            "arquivo": p.name,
            "sha256": sha256_of(p),
            "bytes": p.stat().st_size,
            "rows_ou_dims": rows,
        })
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")

    big = [f for f in (shp, geojson, gpkg, csvf, tif) if f.stat().st_size > 1_000_000]
    assert not big, f"fixture > 1MB: {big}"
    print("[fixtures] OK:")
    for entry in manifest["files"]:
        print(f"  {entry['arquivo']:28} {entry['bytes']:>9} B  sha256={entry['sha256'][:12]}…")


if __name__ == "__main__":
    main()
