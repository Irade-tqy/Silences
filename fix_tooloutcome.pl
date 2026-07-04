#!/usr/bin/perl
use strict;
use warnings;

# Fix all tool files that have ToolOutcome but lack inject_messages
my @files = grep { !/mod\.rs|end_task\.rs/ } glob("crates/silences-agent/src/toolcall/*.rs");

foreach my $file (@files) {
    open(my $fh, '<:encoding(UTF-8)', $file) or die "Cannot open $file: $!";
    my $content = do { local $/; <$fh> };
    close($fh);

    next if $content !~ /ToolOutcome\s*\{/;
    next if $content =~ /inject_messages/;

    # For each ToolOutcome block, add fields before the closing })
    $content =~ s/
        (\n\s*\})        # closing brace of struct (with preceding newline and indent)
        (\s*\))          # immediate closing paren
    /
        my $brace = $1;
        my $paren = $2;
        # Only match if this is after ToolOutcome fields (not in other structs)
        # Check if we're inside a ToolOutcome block by looking backwards
        "\n                inject_messages: vec![],\n                defer_rollback: false," . $brace . $paren
    /xeg;

    open(my $out, '>:encoding(UTF-8)', $file) or die "Cannot write $file: $!";
    print $out $content;
    close($out);
    print "Fixed: $file\n";
}
