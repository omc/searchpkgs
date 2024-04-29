# searchpkgs

Nix packages for every version of [several of] the search engines we use at OMC.

```nix
{
    inputs.searchpkgs.url = "github:omc/searchpkgs";
    outputs = { searchpkgs, ... }: {
        # todo: show how to use this in a module or devShell
    };
}
```

Elasticsearch (and thus OpenSearch) use some Java security permissions that are
not particularly ergonomic when coupled with the readonly nix store paradigm.
So we also provide wrapper scripts to initialize and start each.

These function similarly to the approach taken in the nixpkgs module for each of
these services: use the contents of the store to populate a working directory,
and run the service relative to that directory. If you've ever tried running
OpenSearch or Elasticsearch from nixpkgs, but gotten an error about the Nix
store being read-only, this may be useful to you as well.

To run Elasticsearch or OpenSearch, set `ELASTICSEARCH_HOME` or
`OPENSEARCH_HOME`, respectively, in your environment. You can also set a
separate `ELASTICSEARCH_CONF_PATH` and `OPENSEARCH_CONF_PATH` to give a
different location to the config directory and its contents.

Furthermore there is a `OPENSEARCH_JAVA_OPTS` and `ELASTICSEARCH_JAVA_OPTS`
variables, for options like memory size or GC settings.

Note that this project is provide AS-IS, without support; these packages are not
intended for production usage. OMC provides paid production managed search at
bonsai.io, and you're welcome to chat with us there if you're in need of
production Elasticsearch or OpenSearch support. While these packages are not our
production packages, they follow parallel ideas, and are useful for us in
various development and testing contexts.

Version updates should be kept up to date automatically via GitHub actions. Pull
requests for supporting other search engines are welcome! We're not partisan
about any one technology, what you see is just a useful place for us to get
started.
