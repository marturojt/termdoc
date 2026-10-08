# 0011. Config and log formats are highlighted, not parsed

**Status:** Accepted (M1)

## Context

A parser reports data, and the data of a config file is the least interesting thing in it: comments are what explain the settings, and every parser's event stream drops them and normalises the rest. A viewer that shows a different document than the one on disk is worse than `cat`.

## Decision

YAML, TOML and logs are classified one line at a time (`termdoc_core::highlight`): the text goes out byte for byte as borrowed slices and only its colouring is decided. XML uses `quick-xml`, but only to *delimit* events; the original bytes are what is shown. JSON is the exception and is re-indented on purpose, because minified JSON is one unreadable line.

## Consequences

Output is the file on disk, comments included. It truly streams, needs no parser dependency, and costs a few hundred lines per format. The price is that these readers are lexers, not validators: invalid YAML is coloured like valid YAML and `--explain` is where 'is this really YAML' is answered. Known mis-colourings are listed in each module header and none loses text.
