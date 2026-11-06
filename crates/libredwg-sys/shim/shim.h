/* SPDX-FileCopyrightText: 2026 Jamie Jahn
   SPDX-License-Identifier: GPL-3.0-or-later

   The part of LibreDWG that dwg2gltf uses: dwg.h and dwg_api.h, plus a
   memory-based decoder and encoder (Rust reads and writes the files, so any
   path works on any platform). */

#ifndef DWG2GLTF_SHIM_H
#define DWG2GLTF_SHIM_H

#include "dwg.h"
#include "dwg_api.h"

/* Decode a DWG file image into dwg (zeroed first). data holds size bytes
   plus one trailing 0 byte. Returns LibreDWG's error bits; a value of
   DWG_ERR_CRITICAL or more means failure. Release dwg with dwg_free. */
int dwg2gltf_decode (unsigned char *data, size_t size, Dwg_Data *dwg);

/* A text field of a decoded object (UCS-2 in r2007+ files, else in the
   drawing's codepage) as a new UTF-8 string, or NULL. Free it with
   dwg2gltf_free. */
char *dwg2gltf_text (const Dwg_Data *dwg, const char *text);

void dwg2gltf_free (void *ptr);

/* A new drawing (r2010, r2013 or r2018) with the header values AutoCAD
   (r2010, r2018) and ODA (r2013) use, and INSUNITS set.
   Release it with dwg_free. */
Dwg_Data *dwg2gltf_new (Dwg_Version_Type version, BITCODE_BS insunits);

/* The handle value of an object or entity (any Dwg_Object_* / Dwg_Entity_*
   or its parent). */
BITCODE_RLL dwg2gltf_handle (const void *obj);

/* A dictionary under key in dict, owned by it (as the NOD entries are). */
Dwg_Object_DICTIONARY *dwg2gltf_add_dictionary (Dwg_Object_DICTIONARY *dict,
                                                const char *key);

/* Encode dwg into memory: *data (free with dwg2gltf_free) of *size bytes.
   Returns LibreDWG's error bits; DWG_ERR_CRITICAL or more means failure. */
int dwg2gltf_encode (Dwg_Data *dwg, unsigned char **data, size_t *size);

#endif
