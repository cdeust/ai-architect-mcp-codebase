#!/usr/bin/env python3
"""Compares a measured run with expected.json; exit 0 iff every value is equal, else 1 (deviations
listed). Scoring logic promoted unchanged from tasks/axon-main-b4714a8-20260930/{score,sitescore}.py:
definitions/callers by Counter matching of (file::qualified_name, line); sites by (file, line)
against the per-site rows; semantic hash over definitions, all Calls edges, response_of call sites
and the impact callers.
usage: compare.py ORACLE_JSON EXPECTED_JSON RUN_DIR SITES_DIR"""
import hashlib
import json
import pathlib
import sys
from collections import Counter

LABELS = ('Function', 'Method')
TARGETS = ('Function', 'Method', 'StdlibSymbol')
TARGET_NAME = 'src/lib.rs::TaskSet::response_of'
KANI_HELPER = 'kani/response_bounds.rs::two_tasks_terminate'


def rows(directory, tag):
    pages = json.loads((directory / (tag + '-pages.json')).read_text())
    return [row for page in pages for row in page['rows']]


def payload(directory, tag):
    record = json.loads((directory / (tag + '.json')).read_text())
    return json.loads(record['response']['result']['content'][0]['text'])


def match(expected, actual):
    """(found, false) of Counter-matching `actual` against `expected`."""
    want, got = Counter(expected), Counter(actual)
    return sum((want & got).values()), sum((got - want).values())


def definitions_score(directory, oracle):
    wanted = [(f['file'] + '::' + f['qualified_name'], f['line']) for f in oracle['functions']]
    actual = [(r[0], int(r[2])) for label in LABELS for r in rows(directory, label)]
    found, false = match(wanted, actual)
    return {'found': found, 'of': len(wanted), 'false': false}


def callers_score(oracle, callers):
    by_id = {f['id']: f['file'] + '::' + f['qualified_name'] for f in oracle['functions']}
    wanted = [by_id[c] for c in oracle['response_of_callers']]
    found, false = match(wanted, [c['qualified_name'] for c in callers])
    return {'found': found, 'of': len(wanted), 'false': false}


def site_key(row):
    return (row[0].split('::')[0], int(row[2]))


def is_target_callee(callee):
    return callee.replace('::', '.').split('.')[-1] == 'response_of'


def sites_score(oracle, sites):
    """A site counts only when its single per-site row names the oracle target."""
    by_id = {}
    for label in TARGETS:
        for r in rows(sites, f'site-rows-{label}'):
            by_id.setdefault(r[0], []).append(r[4])
    kept = [s for s in rows(sites, 'callsites-all') if is_target_callee(s[1])]
    true_keys = [site_key(s) for s in kept if by_id.get(s[0]) == [TARGET_NAME]]
    wrong = sum(1 for s in kept if s[0] in by_id and by_id[s[0]] != [TARGET_NAME])
    wanted = [(c['file'], c['line']) for c in oracle['response_of_callsites']]
    found, not_oracle = match(wanted, true_keys)
    return {'found': found, 'of': len(wanted), 'false': wrong + not_oracle}


def semantic_hash(directory, callers):
    edges = sorted(tuple(r) for a in LABELS for b in LABELS for r in rows(directory, f'Calls_{a}_{b}'))
    semantic = {
        'definitions': sorted((r[0], int(r[2])) for label in LABELS for r in rows(directory, label)),
        'edges': edges, 'response_callsites': sorted(rows(directory, 'response-callsites')),
        'impact_callers': sorted(c['qualified_name'] for c in callers)}
    canonical = json.dumps(semantic, sort_keys=True, separators=(',', ':')).encode()
    return hashlib.sha256(canonical).hexdigest()


def measured(oracle, run, sites):
    pages = json.loads((run / 'impact-pages.json').read_text())
    callers = [c for page in pages for c in page['callers']]
    status = payload(run, 'status')
    contexts = dict(Counter(c['context'] for c in callers))
    helper = [c['context'] for c in callers if c['qualified_name'] == KANI_HELPER]
    return {
        'definitions': definitions_score(run, oracle),
        'callers': callers_score(oracle, callers),
        'sites': sites_score(oracle, sites),
        'node_count': status['node_count'], 'edge_count': status['edge_count'],
        'call_site_target_count': status['call_site_target_count'],
        'semantic_sha256': semantic_hash(run, callers),
        'impact_response_of': contexts,
        'two_tasks_terminate_context': helper[0] if len(helper) == 1 else f'absent-or-repeated({len(helper)})'}


def main():
    oracle_path, expected_path, run, sites = sys.argv[1:5]
    oracle = json.loads(pathlib.Path(oracle_path).read_text())
    expected = json.loads(pathlib.Path(expected_path).read_text())
    actual = measured(oracle, pathlib.Path(run), pathlib.Path(sites))
    deviations = 0
    for key, want in actual.items():
        exp = expected[key]
        verdict = 'ok' if exp == want else 'DEVIATION'
        deviations += exp != want
        print(f'{verdict:9} {key}: expected {json.dumps(exp, sort_keys=True)} measured {json.dumps(want, sort_keys=True)}')
    print(f'oracle replay: {deviations} deviation(s)')
    return 1 if deviations else 0


if __name__ == '__main__':
    sys.exit(main())
