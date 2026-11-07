# Fonts

`osifont-subset.woff`: [osifont](https://github.com/hikikomori82/osifont)
(commit `2e9aa86`, file `osifont.ttf`), lettering after ISO 3098 as on
technical drawings, by its contributors. GNU GPL version 3 with the GPL font
exception. Cut down to Basic Latin and a few drawing signs (° ± ² ³ × ø):

```sh
python3 -m fontTools.subset osifont.ttf --flavor=woff --layout-features='*' \
    --unicodes='U+0020-007E,U+00B0,U+00B1,U+00B2,U+00B3,U+00D7,U+00F8,U+2009,U+202F,U+2212,U+2013' \
    --output-file=osifont-subset.woff
```

The page uses it for its name and for the dimensions drawn on the model.
