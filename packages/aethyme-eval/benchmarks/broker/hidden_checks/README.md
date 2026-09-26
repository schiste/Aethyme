Hidden checks. The scorer copies each file into the root of a scratch
checkout of an arm's final landing ref and runs it with
`python3 -m unittest -q <module>`. Agents never see these files.
