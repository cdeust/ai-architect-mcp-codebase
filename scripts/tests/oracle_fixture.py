#!/usr/bin/env python3
"""Builds a tiny self-consistent oracle replay fixture under DIR for test_gate.sh:
DIR/oracle.json, DIR/expected.json (from the measured values), DIR/run, DIR/sites.
usage: oracle_fixture.py COMPARE_DIR DIR"""
import json
import pathlib
import sys

sys.path.insert(0, sys.argv[1])
import compare  # noqa: E402

root = pathlib.Path(sys.argv[2])
run, sites = root / 'run', root / 'sites'
run.mkdir(parents=True)
sites.mkdir()

functions = [
    {'id': 'f1', 'file': 'src/lib.rs', 'qualified_name': 'TaskSet::response_of', 'line': 10},
    {'id': 'f2', 'file': 'tests/a.rs', 'qualified_name': 't1', 'line': 5},
    {'id': 'f3', 'file': 'kani/response_bounds.rs', 'qualified_name': 'two_tasks_terminate', 'line': 175},
]
oracle = {'functions': functions, 'response_of_callers': ['f2', 'f3'],
          'response_of_callsites': [{'file': 'tests/a.rs', 'line': 6},
                                    {'file': 'kani/response_bounds.rs', 'line': 176}]}
(root / 'oracle.json').write_text(json.dumps(oracle))


def pages(directory, tag, rows):
    (directory / (tag + '-pages.json')).write_text(json.dumps([{'rows': rows, 'truncated': False}]))


def record(directory, tag, body):
    text = json.dumps(body)
    (directory / (tag + '.json')).write_text(json.dumps(
        {'response': {'result': {'content': [{'type': 'text', 'text': text}]}}}))


pages(run, 'Function', [['src/lib.rs::TaskSet::response_of', 'response_of', '10', '12'],
                        ['tests/a.rs::t1', 't1', '5', '8'],
                        ['kani/response_bounds.rs::two_tasks_terminate', 'x', '175', '180']])
pages(run, 'Method', [])
for a in ('Function', 'Method'):
    for b in ('Function', 'Method'):
        pages(run, f'Calls_{a}_{b}', [])
pages(run, 'response-callsites', [['tests/a.rs::t1#1', 'ts.response_of', '6', 'true'],
                                  ['kani/response_bounds.rs::two_tasks_terminate#1', 'ts.response_of', '176', 'true']])
(run / 'impact-pages.json').write_text(json.dumps([{'callers': [
    {'qualified_name': 'tests/a.rs::t1', 'context': 'test'},
    {'qualified_name': 'kani/response_bounds.rs::two_tasks_terminate', 'context': 'proof'}]}]))
record(run, 'status', {'node_count': 7, 'edge_count': 9, 'call_site_target_count': 2})
target = 'src/lib.rs::TaskSet::response_of'
pages(sites, 'site-rows-Function', [])
pages(sites, 'site-rows-StdlibSymbol', [])
pages(sites, 'site-rows-Method', [['tests/a.rs::t1#1', '6', 'ts.response_of', 'true', target, 'm', '0.9'],
                                  ['kani/response_bounds.rs::two_tasks_terminate#1', '176', 'ts.response_of', 'true', target, 'm', '0.9']])
pages(sites, 'callsites-all', [['tests/a.rs::t1#1', 'ts.response_of', '6', 'true'],
                               ['kani/response_bounds.rs::two_tasks_terminate#1', 'ts.response_of', '176', 'true']])

expected = compare.measured(oracle, run, sites)
(root / 'expected.json').write_text(json.dumps(expected, indent=1))
