# Minimal gallery

A gallery workload built with the Isoplane handler API, Npgsql, the AWS S3 SDK,
Stb image libraries, and MetadataExtractor. The application has no ASP.NET or
EF Core runtime dependency. `Design/` owns the EF migrations used by both this
sample and `../aspnet-gallery`.

The two implementations share these request and storage conventions:

| Request | Result |
| --- | --- |
| `POST /images` with raw JPEG or PNG bytes | `201` with image metadata and the original's location |
| `GET /images` | Metadata ordered by image ID |
| `GET /images/{id}/original` | The byte-for-byte original |
| `GET /images/{id}/thumbnail` | A PNG thumbnail |

Metadata includes the ID, detected original content type, original and thumbnail
pixel dimensions, and EXIF `make`, `model`, `dateTimeOriginal`, and `orientation`.
Absent EXIF fields are null. The thumbnail preserves the source pixel aspect
ratio, never enlarges an image, and defaults to a maximum edge of 256 pixels.
It records but does not apply EXIF orientation. The PNG thumbnail contains no
source EXIF; the original retains it unchanged.

The PostgreSQL table is `GalleryImages`. S3 keys are the image's UUID without
hyphens followed by `/original` or `/thumbnail.png`. Both objects are uploaded
before metadata is inserted. Failed object uploads attempt to remove both
keys. If the database acknowledgement fails, objects remain because the insert
may have committed. There is no distributed transaction or orphan collector.

Supply these values through Isoplane runtime configuration:

| Key | Meaning |
| --- | --- |
| `ConnectionStrings:Gallery` | PostgreSQL connection string |
| `Gallery:S3:Endpoint` | S3 service URL |
| `Gallery:S3:Region` | Signing region |
| `Gallery:S3:Bucket` | Existing bucket |
| `Gallery:S3:AccessKey` | Access key |
| `Gallery:S3:SecretKey` | Secret key |
| `Gallery:ThumbnailEdge` | Optional positive maximum thumbnail edge |

PostgreSQL uses verified TLS and disables connection pooling. Configure trusted
roots and allowed endpoints through the local development manifest. Credentials
are read when a request runs and are not part of the prepared application.
The S3 SDK uses path-style requests and retains its signing and checksum defaults.

Build configurations are `IsoplaneInterpretedDebug` and `IsoplaneNativeAotRelease`.
Apply the schema from the separate design project with `dotnet ef database update
--project Design --connection "<your PostgreSQL connection string>"` after
installing the matching `dotnet-ef` 10.0 tool. The guest does not apply migrations.

Image dependencies are pinned in the project:
[StbImageSharp](https://github.com/StbSharp/StbImageSharp),
[StbImageWriteSharp](https://github.com/StbSharp/StbImageWriteSharp),
[StbImageResizeSharp](https://github.com/StbSharp/StbImageResizeSharp), and
[MetadataExtractor](https://github.com/drewnoakes/metadata-extractor-dotnet).
The Stb ports use permissive/public-domain licensing and MetadataExtractor uses
Apache 2.0. The small Stb resize port has a lower maintenance cadence than the
larger image frameworks; this sample keeps processing behind one shared module.

`verify.py` checks a published guest against disposable PostgreSQL 16 and Garage
2.3.0 containers. Install `verification-requirements.txt` in a virtual environment,
then pass `--module`, `--mode`, `--isoplane`, `--schema`, and `--records`. Generate the
SQL supplied to `--schema` with `dotnet ef migrations script --project Design
--output <path>`. The verifier creates a deterministic JPEG with actual EXIF,
checks the response and independent PostgreSQL/S3 reads, decodes the thumbnail
with Pillow, and removes its containers on exit. Its startup/request deadlines
only detect hangs. It requires Docker and OpenSSL on the machine running it.

The sample declares a 512 MiB maximum guest memory. This ceiling keeps the
workload comparison consistent; it is not a claim about memory actually used.
