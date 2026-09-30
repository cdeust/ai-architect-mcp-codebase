#!/usr/bin/env python3
"""Per-site rows (Calls_CallSite_*) and every CallSite of an already-measured graph, read-only.
Promoted from tasks/axon-main-b4714a8-20260930/sitecollect.py, trimmed to the listings compare.py
reads (site rows, callsites-all). Each listing is walked with next_offset until `truncated` is
false and its row count is checked against count(*) of the same pattern; a mismatch aborts.
usage: sitecollect.py BINARY GRAPH_DIR OUT_DIR   (graph built by BINARY; OUT_DIR must not exist)"""
import json
import pathlib
import sys

from measure import Session

TARGETS = ('Function', 'Method', 'StdlibSymbol')


def target_key(label):
    """StdlibSymbol nodes carry no qualified_name; they are keyed by id."""
    return 't.id' if label == 'StdlibSymbol' else 't.qualified_name'


def listings():
    specs = []
    for label in TARGETS:
        key = target_key(label)
        specs.append((f'(c:CallSite)-[r:Calls_CallSite_{label}]->(t:{label})',
                      f'c.id, c.line, c.callee_name, c.is_resolved, {key}, '
                      'r.resolution_method, r.confidence', f'c.id, {key}', f'site-rows-{label}'))
    specs.append(('(c:CallSite)', 'c.id, c.callee_name, c.line, c.is_resolved', 'c.id',
                  'callsites-all'))
    return specs


def walk(session, graph, spec):
    pattern, returns, order, tag = spec
    query = f'MATCH {pattern} RETURN {returns} ORDER BY {order}'
    session.pages('query_graph', {**graph, 'query': query, 'format': 'tabular'}, tag)
    pages = json.loads((session.directory / f'{tag}-pages.json').read_text())
    if pages[-1]['truncated']:
        raise RuntimeError(f'{tag}: last page still truncated')
    walked = sum(len(p['rows']) for p in pages)
    count = session.call('query_graph', {**graph, 'format': 'tabular',
                         'query': f'MATCH {pattern} RETURN count(*)'}, f'{tag}-count')
    if int(count['rows'][0][0]) != walked:
        raise RuntimeError(f'{tag}: walked {walked} != count {count["rows"][0][0]}')


def main():
    out = pathlib.Path(sys.argv[3]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    graph = {'graph_path': str(pathlib.Path(sys.argv[2]).resolve())}
    session = Session(sys.argv[1], out)
    try:
        for spec in listings():
            walk(session, graph, spec)
    finally:
        session.close()


if __name__ == '__main__':
    main()
