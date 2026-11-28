"""Aggregate-derived acceptance data, not historical request data."""

DEEPSEEK = "deepseek-v4.1-flash"
ISSUE_TITLE = "DeepSeek 180 条 · 隔离验收"
OTHER_TITLE = "Runner 同口径 · 合成对照"
CASES = {
    "Verify the 180-request DeepSeek estimate.": ISSUE_TITLE,
    "Verify the remaining synthetic Runner requests.": OTHER_TITLE,
}
# Inclusive input, hit, output. The first row comes from the supplied
# aggregate. The second is synthetic and makes the combined Runner total
# equal the supplied reference amount without claiming a historical split.
GROUPS = {
    ISSUE_TITLE: (180, (1_211_884, 806_656, 104_232)),
    OTHER_TITLE: (240, (37_643_984, 9_193_404, 895_768)),
}


def allocate(total, index, count):
    return total // count + (index < total % count)


def samples(title):
    count, sums = GROUPS[title]
    for i in range(count):
        prompt, hit, output = (allocate(n, i, count) for n in sums)
        if i % 2:
            yield DEEPSEEK, dict(
                total_tokens=prompt + output, prompt_tokens=prompt, completion_tokens=output,
                prompt_cache_hit_tokens=hit, prompt_cache_miss_tokens=prompt - hit,
            )
        else:
            yield DEEPSEEK, dict(
                total_tokens=prompt + output, input_tokens=prompt, output_tokens=output,
                input_tokens_details={"cached_tokens": hit},
            )
    if title == ISSUE_TITLE:
        yield "gpt-6.2-sol", dict(total_tokens=1050, input_tokens=1000, output_tokens=50)
        yield "gpt-6.2-sol", dict(
            total_tokens=272_011, input_tokens=272_001, output_tokens=10,
            input_tokens_details={"cached_tokens": 200_000, "cache_write_tokens": 10_000},
            output_tokens_details={"reasoning_tokens": 7},
        )
