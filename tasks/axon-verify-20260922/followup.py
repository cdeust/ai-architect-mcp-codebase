#!/usr/bin/env python3
"""Read-only follow-up calls against an already-measured graph, recorded into a
separate directory so the measured run's own transcripts stay untouched.
usage: followup.py BINARY GRAPH_DIR OUT_DIR"""
import json
import pathlib
import sys
from measure import Session

KANI = "kani/response_bounds.rs"


def unresolved_in_kani(session, graph):
    query = ("MATCH (n:CallSite) WHERE n.id STARTS WITH '" + KANI + "' "
             "RETURN n.id, n.callee_name, n.line, n.is_resolved, n.unresolved_reason "
             "ORDER BY n.id")
    return session.send('query_graph', {**graph, 'query': query, 'format': 'tabular',
                                        'limit': 500}, 'kani-callsites')


def main():
    out = pathlib.Path(sys.argv[3]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    graph = {'graph_path': str(pathlib.Path(sys.argv[2]).resolve())}
    session = Session(sys.argv[1], out)
    try:
        unresolved_in_kani(session, graph)
        session.send('index_status', graph, 'index-status')
        for name in sys.argv[4:]:
            tag = 'impact-' + name.replace('/', '_').replace(':', '_')
            session.send('get_impact', {**graph, 'qualified_name': name}, tag)
    finally:
        session.close()


if __name__ == '__main__':
    main()
