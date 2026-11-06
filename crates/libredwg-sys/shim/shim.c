/* SPDX-FileCopyrightText: 2026 Jamie Jahn
   SPDX-License-Identifier: GPL-3.0-or-later */

#include "config.h"
#include <stdlib.h>
#include <string.h>
#include "bits.h"
#include "decode.h"
#include "encode.h"
#include "shim.h"

int
dwg2gltf_decode (unsigned char *data, size_t size, Dwg_Data *dwg)
{
  /* what dwg_read_file does after reading the file, minus the paper space
     viewport fixups */
  Bit_Chain dat = { 0 };
  memset (dwg, 0, sizeof (Dwg_Data));
  dat.chain = data;
  dat.size = size;
  return dwg_decode (&dat, dwg);
}

char *
dwg2gltf_text (const Dwg_Data *dwg, const char *text)
{
  char *utf8;
  if (!text)
    return NULL;
  if (IS_FROM_TU_DWG (dwg))
    return bit_convert_TU ((BITCODE_TU)text);
  /* returns text itself when there is nothing to convert */
  utf8 = bit_TV_to_utf8 (text, dwg->header.codepage);
  return utf8 == text ? strdup (text) : utf8;
}

void
dwg2gltf_free (void *ptr)
{
  free (ptr);
}

Dwg_Data *
dwg2gltf_new (Dwg_Version_Type version, BITCODE_BS insunits)
{
  Dwg_Data *dwg = dwg_new_Document (version, 0, 0);
  if (!dwg)
    return NULL;
  dwg->header.from_version = version;
  dwg->header.zero_one_or_three = 3;
  /* r2013 needs maint_version > 3, else the classes section has no hsize
     and the string stream is misplaced */
  dwg->header.dwg_version = version == R_2010 ? 29 : 33;
  dwg->header.maint_version = version == R_2010   ? 37
                              : version == R_2013 ? 55
                                                  : 4;
  dwg->header_vars.INSUNITS = insunits;
  return dwg;
}

BITCODE_RLL
dwg2gltf_handle (const void *obj)
{
  int error;
  Dwg_Object *o = dwg_obj_generic_to_object (obj, &error);
  return o && !error ? o->handle.value : 0;
}

Dwg_Object_DICTIONARY *
dwg2gltf_add_dictionary (Dwg_Object_DICTIONARY *dict, const char *key)
{
  int error;
  Dwg_Object *parent = dwg_obj_generic_to_object (dict, &error);
  Dwg_Data *dwg = parent && !error ? parent->parent : NULL;
  BITCODE_RLL parent_ref;
  Dwg_Object_DICTIONARY *sub;
  Dwg_Object_Object *o;
  if (!dwg)
    return NULL;
  parent_ref = parent->handle.value; /* parent may move */
  sub = dwg_add_DICTIONARY (dwg, NULL, NULL, 0);
  if (!sub)
    return NULL;
  o = sub->parent;
  o->ownerhandle = dwg_add_handleref (dwg, 4, parent_ref,
                                      &dwg->object[o->objid]);
  o->num_reactors = 1;
  o->reactors = (BITCODE_H *)calloc (1, sizeof (BITCODE_H));
  if (!o->reactors)
    return NULL;
  o->reactors[0] = dwg_add_handleref (dwg, 4, parent_ref, NULL);
  dwg_add_DICTIONARY_item (dict, key, dwg->object[o->objid].handle.value);
  return sub;
}

int
dwg2gltf_encode (Dwg_Data *dwg, unsigned char **data, size_t *size)
{
  /* what dwg_write_file does, into memory. dwg_add_Document marks the
     drawing as a DXF import (0.14 also marked it as a JSON import; master
     no longer does, GH #1499). The JSON hacks (xdata cut at 32 KB, layer
     flags taken as given) must not apply, so the flag stays cleared here. */
  const unsigned opts = dwg->opts;
  Bit_Chain dat = { 0 };
  int error;
  dwg->opts &= ~DWG_OPTS_INJSON;
  dat.opts = dwg->opts;
  dat.version = dwg->header.version;
  dat.from_version = dwg->header.version;
  dat.codepage = dwg->header.codepage;
  error = dwg_encode (dwg, &dat);
  dwg->opts = opts;
  if (error >= DWG_ERR_CRITICAL || !dat.chain)
    {
      free (dat.chain);
      *data = NULL;
      *size = 0;
      return error | DWG_ERR_CRITICAL;
    }
  *data = dat.chain;
  *size = dat.size;
  return error;
}
